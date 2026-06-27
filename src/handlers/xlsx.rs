//! Optimized XLSX handler with hierarchical blocks.
//!
//! Instead of creating a block per cell (which doesn't scale), this handler
//! creates a hierarchical structure:
//!   - Sheet level: one block per worksheet
//!   - Row range level: blocks for row ranges (e.g., rows 0-99, 100-199)
//!   - Cell level: only hydrated on demand via read()
//!
//! This reduces block count from O(rows * cols) to O(sheets + row_ranges).
//!
//! Cell *content* is resolved the same way an OOXML spreadsheet stores it:
//!   - `t="s"`        — `<v>` holds an index into `xl/sharedStrings.xml`
//!   - `t="inlineStr"`— text lives in `<is>…<t>…</t>…</is>` on the cell itself
//!   - `t="str"`      — `<v>` holds a formula's string result, verbatim
//!   - `t="b"`        — `<v>` holds a boolean (`1`/`0`)
//!   - otherwise      — `<v>` holds a number / date serial, verbatim
//!
//! Resolving these is what lets the discovery loop (scan previews, grep, and
//! row-range read) reach actual cell values rather than only sheet structure.

use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use zip::ZipArchive;

use super::xml_unescape;
use crate::model::{Attr, DocNode};

/// XLSX worksheet metadata.
#[derive(Debug, Clone)]
pub struct SheetInfo {
    pub name: String,
    pub part_name: String,
    pub row_count: usize,
    pub col_count: usize,
}

/// Row range for lazy loading.
#[derive(Debug, Clone)]
pub struct RowRange {
    pub start: usize,
    pub end: usize,
    pub sheet_part: String,
}

/// XLSX-specific extraction result with hierarchical structure.
pub struct XlsxExtractResult {
    pub sheets: Vec<SheetInfo>,
    pub structure: Vec<DocNode>,
    pub row_ranges: Vec<RowRange>,
}

/// Number of rows per lazily-loaded range block.
const RANGE_SIZE: usize = 100;

/// Parse XLSX and extract sheet metadata without loading every cell.
///
/// Each row-range block's preview carries the resolved values of the range's
/// first row, mirroring the CSV handler so `grep`/`scan` surface real content.
pub fn scan_xlsx(bytes: &[u8]) -> Result<XlsxExtractResult> {
    let names = entry_names(bytes)?;
    let shared = load_shared_strings(bytes).unwrap_or_default();

    // Find worksheet parts (deterministic order).
    let mut sheet_parts: Vec<String> = names
        .iter()
        .filter(|n| {
            n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .cloned()
        .collect();
    sheet_parts.sort();

    let mut sheets = Vec::new();
    let mut structure = Vec::new();
    let mut row_ranges = Vec::new();

    for (idx, part) in sheet_parts.iter().enumerate() {
        let part_bytes = read_part(bytes, part)?;
        let content = std::str::from_utf8(&part_bytes)
            .with_context(|| format!("invalid UTF-8 in {}", part))?;

        let offsets = row_offsets(content);
        let row_count = offsets.len();
        let col_count = offsets
            .first()
            .map(|&off| parse_cells(row_xml_at(content, off), &shared).len())
            .unwrap_or(0);

        let name = format!("Sheet{}", idx + 1);
        let sheet_info = SheetInfo {
            name: name.clone(),
            part_name: part.clone(),
            row_count,
            col_count,
        };

        // Sheet-level block.
        let sheet_id = format!("sheet[{}]", idx);
        structure.push(DocNode {
            id: sheet_id.clone(),
            tag: "_sheet".to_string(),
            attrs: vec![
                Attr {
                    name: "name".to_string(),
                    value: name.clone(),
                },
                Attr {
                    name: "rows".to_string(),
                    value: row_count.to_string(),
                },
                Attr {
                    name: "cols".to_string(),
                    value: col_count.to_string(),
                },
            ],
            text: Some(format!("{} ({} rows × {} cols)", name, row_count, col_count)),
            children: Vec::new(), // Cells loaded on demand
        });

        // Row-range blocks (RANGE_SIZE rows per range).
        let mut row = 0;
        while row < row_count {
            let range_end = (row + RANGE_SIZE).min(row_count);
            let range_id = format!("{}.rows[{}-{}]", sheet_id, row, range_end - 1);

            // Preview the range's first row so grep/scan see real cell values.
            let first_row_values = parse_cells(row_xml_at(content, offsets[row]), &shared)
                .into_iter()
                .map(|(_r, v)| v)
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            let preview = if first_row_values.is_empty() {
                format!("Rows {}-{}", row, range_end - 1)
            } else {
                format!("Rows {}-{}: {}", row, range_end - 1, first_row_values)
            };

            structure.push(DocNode {
                id: range_id,
                tag: "_row_range".to_string(),
                attrs: vec![
                    Attr {
                        name: "start".to_string(),
                        value: row.to_string(),
                    },
                    Attr {
                        name: "end".to_string(),
                        value: (range_end - 1).to_string(),
                    },
                    Attr {
                        name: "sheet".to_string(),
                        value: part.clone(),
                    },
                    Attr {
                        name: "sheet_idx".to_string(),
                        value: idx.to_string(),
                    },
                ],
                text: Some(preview),
                children: Vec::new(), // Cells loaded on demand
            });

            row_ranges.push(RowRange {
                start: row,
                end: range_end,
                sheet_part: part.clone(),
            });

            row = range_end;
        }

        sheets.push(sheet_info);
    }

    Ok(XlsxExtractResult {
        sheets,
        structure,
        row_ranges,
    })
}

/// Load cells for an ordinal row range, `[start_row, end_row]` inclusive.
///
/// Returns one `row` node per source `<row>` element in the range, each holding
/// a `c` (cell) child per cell with its fully resolved value as text. `sheet_idx`
/// is used only to build human-stable node ids (`sheet[i].row[n].<col>`).
pub fn load_row_range(
    bytes: &[u8],
    sheet_part: &str,
    sheet_idx: usize,
    start_row: usize,
    end_row: usize,
) -> Result<Vec<DocNode>> {
    let shared = load_shared_strings(bytes).unwrap_or_default();
    let part_bytes = read_part(bytes, sheet_part)?;
    let content = std::str::from_utf8(&part_bytes)
        .with_context(|| format!("invalid UTF-8 in {}", sheet_part))?;

    let mut nodes = Vec::new();

    for (ordinal, &off) in row_offsets(content).iter().enumerate() {
        if ordinal < start_row || ordinal > end_row {
            continue;
        }
        let row_xml = row_xml_at(content, off);
        let row_num = attr_value(row_xml, "r")
            .map(|s| s.to_string())
            .unwrap_or_else(|| (ordinal + 1).to_string());

        let mut row_node = DocNode {
            id: format!("sheet[{}].row[{}]", sheet_idx, ordinal),
            tag: "row".to_string(),
            attrs: vec![Attr {
                name: "r".to_string(),
                value: row_num,
            }],
            text: None,
            children: Vec::new(),
        };

        for (col_idx, (cell_ref, value)) in parse_cells(row_xml, &shared).into_iter().enumerate() {
            let col = col_letters(&cell_ref, col_idx);
            row_node.children.push(DocNode {
                id: format!("sheet[{}].row[{}].{}", sheet_idx, ordinal, col),
                tag: "c".to_string(),
                attrs: vec![Attr {
                    name: "r".to_string(),
                    value: if cell_ref.is_empty() {
                        col.clone()
                    } else {
                        cell_ref.clone()
                    },
                }],
                text: Some(value),
                children: Vec::new(),
            });
        }

        nodes.push(row_node);
    }

    Ok(nodes)
}

/// Byte offsets of each `<row …>` element in document order.
fn row_offsets(content: &str) -> Vec<usize> {
    content.match_indices("<row ").map(|(i, _)| i).collect()
}

/// The full XML of the row element beginning at `start`, handling the
/// self-closing (`<row …/>`, an empty row) and `<row …>…</row>` forms.
fn row_xml_at(content: &str, start: usize) -> &str {
    let rest = &content[start..];
    let open_end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
    if rest[..open_end].ends_with("/>") {
        return &rest[..open_end];
    }
    match rest.find("</row>") {
        Some(close) => &rest[..close + "</row>".len()],
        None => &rest[..open_end],
    }
}

/// Parse the cells of a single row's XML into `(cell_ref, resolved_value)`,
/// in document order. Empty / value-less cells yield an empty value.
fn parse_cells(row_xml: &str, shared: &[String]) -> Vec<(String, String)> {
    let mut cells = Vec::new();
    for (c_start, _) in row_xml.match_indices("<c ") {
        let rest = &row_xml[c_start..];
        let open_end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
        let open_tag = &rest[..open_end];
        let cell_ref = attr_value(open_tag, "r").unwrap_or("").to_string();
        let t = attr_value(open_tag, "t");

        let cell_xml = if open_tag.ends_with("/>") {
            open_tag
        } else {
            match rest.find("</c>") {
                Some(close) => &rest[..close + "</c>".len()],
                None => open_tag,
            }
        };

        cells.push((cell_ref, cell_value(cell_xml, t, shared)));
    }
    cells
}

/// Resolve a cell's display value from its XML and `t` (cell-type) attribute.
fn cell_value(cell_xml: &str, t: Option<&str>, shared: &[String]) -> String {
    match t {
        Some("s") => inner_tag(cell_xml, "v")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .and_then(|i| shared.get(i).cloned())
            .unwrap_or_default(),
        Some("inlineStr") => extract_t_text(cell_xml),
        Some("str") => inner_tag(cell_xml, "v")
            .map(|v| xml_unescape(&v))
            .unwrap_or_default(),
        Some("b") => match inner_tag(cell_xml, "v").as_deref().map(str::trim) {
            Some("1") => "TRUE".to_string(),
            Some("0") => "FALSE".to_string(),
            _ => String::new(),
        },
        _ => inner_tag(cell_xml, "v")
            .map(|v| xml_unescape(&v))
            .unwrap_or_default(),
    }
}

/// Column letters for a cell, from its `r` ref (`"D5"` → `"D"`); falls back to
/// a positional letter derived from `col_idx` when the ref is absent.
fn col_letters(cell_ref: &str, col_idx: usize) -> String {
    let letters: String = cell_ref.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() {
        column_name(col_idx)
    } else {
        letters
    }
}

/// Bijective base-26 spreadsheet column name for a zero-based index (0 → A,
/// 25 → Z, 26 → AA).
fn column_name(mut idx: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (idx % 26) as u8);
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap_or_default()
}

/// Load `xl/sharedStrings.xml` into an index-addressable table of plain text.
/// Each `<si>` is flattened (rich-text runs concatenated). Missing table → empty.
fn load_shared_strings(bytes: &[u8]) -> Result<Vec<String>> {
    let names = entry_names(bytes)?;
    if !names.iter().any(|n| n == "xl/sharedStrings.xml") {
        return Ok(Vec::new());
    }
    let part = read_part(bytes, "xl/sharedStrings.xml")?;
    let content =
        std::str::from_utf8(&part).with_context(|| "invalid UTF-8 in xl/sharedStrings.xml")?;

    let mut out = Vec::new();
    let mut rest = content;
    while let Some(p) = rest.find("<si>").or_else(|| rest.find("<si ")) {
        let after = &rest[p..];
        match after.find("</si>") {
            Some(end) => {
                out.push(extract_t_text(&after[..end]));
                rest = &after[end + "</si>".len()..];
            }
            None => break,
        }
    }
    Ok(out)
}

/// Concatenate the text of every `<t>…</t>` element within `xml`, unescaping
/// XML entities. Covers both plain (`<is><t>…`) and rich (`<r><t>…`) runs.
fn extract_t_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(p) = rest.find("<t") {
        let after = &rest[p + 2..];
        // Guard against `<table>`-style false positives: a `t` element opens
        // with `>`, a space (attrs like xml:space), or a self-close `/`.
        if !(after.starts_with('>') || after.starts_with(' ') || after.starts_with('/')) {
            rest = after;
            continue;
        }
        let gt = match after.find('>') {
            Some(g) => g,
            None => break,
        };
        if after[..gt].ends_with('/') {
            // Self-closing `<t/>` — no text.
            rest = &after[gt + 1..];
            continue;
        }
        let body = &after[gt + 1..];
        match body.find("</t>") {
            Some(end) => {
                out.push_str(&xml_unescape(&body[..end]));
                rest = &body[end + "</t>".len()..];
            }
            None => break,
        }
    }
    out
}

/// Inner text of the first `<tag>…</tag>` in `xml` (tag may carry attributes).
fn inner_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let pos = xml.find(&open)?;
    let after = &xml[pos + open.len()..];
    // Only `>` or a space may follow the tag name (avoids `<value>` for `v`).
    if !(after.starts_with('>') || after.starts_with(' ')) {
        return None;
    }
    let gt = after.find('>')?;
    if after[..gt].ends_with('/') {
        return Some(String::new()); // self-closing
    }
    let body = &after[gt + 1..];
    let close = format!("</{}>", tag);
    let end = body.find(&close)?;
    Some(body[..end].to_string())
}

/// Value of attribute `name` in an opening tag (`r="A1"` → `A1`). The leading
/// space ensures we match a whole attribute name, not a suffix of another.
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!(" {}=\"", name);
    let pos = tag.find(&pat)?;
    let after = &tag[pos + pat.len()..];
    let end = after.find('"')?;
    Some(&after[..end])
}

/// List entry names in the container.
fn entry_names(container: &[u8]) -> Result<Vec<String>> {
    let zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    Ok(zip.file_names().map(|s| s.to_string()).collect())
}

/// Read one entry's decompressed bytes from a zip container.
fn read_part(container: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    let mut f = zip
        .by_name(name)
        .with_context(|| format!("part `{name}` not found in container"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}
