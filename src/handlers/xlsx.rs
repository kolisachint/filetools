//! Optimized XLSX handler with hierarchical blocks.
//!
//! Instead of creating a block per cell (which doesn't scale), this handler
//! creates a hierarchical structure:
//!   - Sheet level: one block per worksheet
//!   - Row range level: blocks for row ranges (e.g., rows 0-99, 100-199)
//!   - Cell level: only hydrated on demand via read()
//!
//! This reduces block count from O(rows * cols) to O(sheets + row_ranges).

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use zip::ZipArchive;

use crate::idmap::{IdMap, NodeLoc};
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
    pub idmap: IdMap,
    pub row_ranges: Vec<RowRange>,
}

/// Parse XLSX and extract sheet metadata without loading all cells.
pub fn scan_xlsx(bytes: &[u8]) -> Result<XlsxExtractResult> {
    let names = entry_names(bytes)?;

    // Find worksheet parts
    let sheet_parts: Vec<String> = names
        .iter()
        .filter(|n| {
            n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .cloned()
        .collect();

    let mut sheets = Vec::new();
    let mut structure = Vec::new();
    let idmap_map = BTreeMap::new();
    let mut row_ranges = Vec::new();

    for (idx, part) in sheet_parts.iter().enumerate() {
        let part_bytes = read_part(bytes, part)?;

        // Parse the sheet to get row/column counts
        let sheet_info = parse_sheet_metadata(&part_bytes, part, idx)?;
        let row_count = sheet_info.row_count;
        let col_count = sheet_info.col_count;

        // Create sheet-level block
        let sheet_id = format!("sheet[{}]", idx);
        structure.push(DocNode {
            id: sheet_id.clone(),
            tag: "_sheet".to_string(),
            attrs: vec![
                Attr {
                    name: "name".to_string(),
                    value: sheet_info.name.clone(),
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
            text: Some(format!(
                "{} ({} rows × {} cols)",
                sheet_info.name, row_count, col_count
            )),
            children: Vec::new(), // Cells loaded on demand
        });

        // Create row-range blocks (100 rows per range)
        let range_size = 100;
        let mut row = 0;
        while row < row_count {
            let range_end = (row + range_size).min(row_count);
            let range_id = format!("{}.rows[{}-{}]", sheet_id, row, range_end - 1);

            structure.push(DocNode {
                id: range_id.clone(),
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
                ],
                text: Some(format!("Rows {}-{}", row, range_end - 1)),
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

    let idmap = IdMap {
        for_hash: String::new(), // Will be set by caller
        map: idmap_map,
    };

    Ok(XlsxExtractResult {
        sheets,
        structure,
        idmap,
        row_ranges,
    })
}

/// Parse sheet XML to extract metadata without loading all cells.
fn parse_sheet_metadata(xml_bytes: &[u8], part_name: &str, sheet_idx: usize) -> Result<SheetInfo> {
    let content = std::str::from_utf8(xml_bytes)
        .with_context(|| format!("invalid UTF-8 in {}", part_name))?;

    // Count rows and columns
    let row_count = content.matches("<row ").count();
    let col_count = if row_count > 0 {
        // Estimate columns from first row
        content
            .split("<row ")
            .nth(1)
            .map(|r| r.matches("<c ").count())
            .unwrap_or(0)
    } else {
        0
    };

    let name = format!("Sheet{}", sheet_idx + 1);

    Ok(SheetInfo {
        name,
        part_name: part_name.to_string(),
        row_count,
        col_count,
    })
}

/// Load cells for a specific row range.
pub fn load_row_range(
    bytes: &[u8],
    sheet_part: &str,
    start_row: usize,
    end_row: usize,
    _for_hash: &str,
) -> Result<(Vec<DocNode>, BTreeMap<String, NodeLoc>)> {
    let part_bytes = read_part(bytes, sheet_part)?;
    let content = std::str::from_utf8(&part_bytes)
        .with_context(|| format!("invalid UTF-8 in {}", sheet_part))?;

    let mut nodes = Vec::new();
    let idmap = BTreeMap::new();

    // Parse rows in the specified range
    for (row_idx, _row_match) in content.match_indices("<row ") {
        if row_idx < start_row || row_idx >= end_row {
            continue;
        }

        let row_num = row_idx + 1; // 1-based
        let row_id = format!("{}_row[{}]", sheet_part, row_num);

        // Extract row content
        if let Some(row_end) = content[row_idx..].find("</row>") {
            let row_xml = &content[row_idx..row_idx + row_end + 6];

            // Create row node
            let mut row_node = DocNode {
                id: row_id.clone(),
                tag: "row".to_string(),
                attrs: vec![Attr {
                    name: "r".to_string(),
                    value: row_num.to_string(),
                }],
                text: None,
                children: Vec::new(),
            };

            // Parse cells in this row
            for (col_idx, _cell_match) in row_xml.match_indices("<c ") {
                let cell_id = format!("{}_cell[{},{}]", sheet_part, row_num, col_idx + 1);

                // Extract cell content
                if let Some(cell_end) = row_xml[col_idx..].find("</c>") {
                    let cell_xml = &row_xml[col_idx..col_idx + cell_end + 4];

                    // Extract cell value
                    let value = if let Some(v_start) = cell_xml.find("<v>") {
                        if let Some(v_end) = cell_xml[v_start..].find("</v>") {
                            &cell_xml[v_start + 3..v_start + v_end]
                        } else {
                            ""
                        }
                    } else {
                        ""
                    };

                    let cell_node = DocNode {
                        id: cell_id.clone(),
                        tag: "c".to_string(),
                        attrs: vec![Attr {
                            name: "r".to_string(),
                            value: format!("{}{}", (b'A' + (col_idx % 26) as u8) as char, row_num),
                        }],
                        text: Some(value.to_string()),
                        children: Vec::new(),
                    };

                    row_node.children.push(cell_node);
                }
            }

            nodes.push(row_node);
        }
    }

    Ok((nodes, idmap))
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
