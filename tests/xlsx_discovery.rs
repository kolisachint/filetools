//! Discovery-loop tests for xlsx: scan previews, grep, and row-range read must
//! reach actual cell values (shared strings, inline strings, and typed cells),
//! not just sheet structure. This is the gap the OOXML calc handler closes so
//! the xlsx loop matches the CSV handler's `rows[a-b]` behaviour.

use std::io::Write;

use filetools_rs::model::GrepOptions;
use filetools_rs::{clear_scan_cache, grep, read, scan};

/// Build a zip container from (name, body) parts.
fn build_zip(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    }
    out
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0"?><Types/>"#;
const WORKBOOK: &str = r#"<?xml version="1.0"?><workbook/>"#;

/// A workbook whose cell text comes from the shared-strings table (the common
/// case Excel/LibreOffice produce).
fn shared_string_xlsx() -> Vec<u8> {
    let shared = r#"<?xml version="1.0"?><sst xmlns="x" count="4">
        <si><t>Region</t></si>
        <si><t>Revenue</t></si>
        <si><t>APAC</t></si>
        <si><t>Northwind Trading</t></si>
    </sst>"#;
    // Row 1: header (shared strings 0,1). Row 2: APAC (shared 2), a number,
    // and a longer shared string in column C.
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="x"><sheetData>
        <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
        <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>42</v></c><c r="C2" t="s"><v>3</v></c></row>
    </sheetData></worksheet>"#;
    build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/sharedStrings.xml", shared),
        ("xl/worksheets/sheet1.xml", sheet),
    ])
}

/// A workbook that stores text inline on the cell (`t="inlineStr"`), no
/// shared-strings table at all.
fn inline_string_xlsx() -> Vec<u8> {
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="x"><sheetData>
        <row r="1"><c r="A1" t="inlineStr"><is><t>Region</t></is></c></row>
        <row r="2"><c r="A2" t="inlineStr"><is><t>Northwind Trading</t></is></c><c r="B2"><v>99</v></c></row>
    </sheetData></worksheet>"#;
    build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/worksheets/sheet1.xml", sheet),
    ])
}

#[test]
fn scan_preview_surfaces_first_row_cell_values() {
    clear_scan_cache();
    let xlsx = shared_string_xlsx();
    let result = scan("book.xlsx", &xlsx).unwrap();

    let range = result
        .blocks
        .iter()
        .find(|b| b.id == "sheet[0].rows[0-1]")
        .expect("row-range block present");
    // The first data row's resolved shared-string headers appear in the preview.
    assert!(
        range.preview.contains("Region") && range.preview.contains("Revenue"),
        "preview should carry resolved cell values, got: {:?}",
        range.preview
    );
}

#[test]
fn grep_matches_shared_string_cell_value() {
    let xlsx = shared_string_xlsx();
    let opts = GrepOptions::default();

    // A value that lives in a non-first column (C2) via the shared table.
    let matches = grep("book.xlsx", &xlsx, "Northwind", &opts).unwrap();
    assert!(!matches.is_empty(), "expected a cell match for 'Northwind'");
    // The match resolves to the row-range block id that `read` accepts.
    assert!(
        matches.iter().any(|m| m.block_id == "sheet[0].rows[0-1]"),
        "match should attribute to the row-range block, got: {:?}",
        matches.iter().map(|m| &m.block_id).collect::<Vec<_>>()
    );
    assert!(matches.iter().all(|m| m.writable), "xlsx is editable");
}

#[test]
fn grep_matches_inline_string_cell_value() {
    let xlsx = inline_string_xlsx();
    let opts = GrepOptions::default();
    let matches = grep("book.xlsx", &xlsx, "Northwind", &opts).unwrap();
    assert!(
        !matches.is_empty(),
        "inline-string cell text must be greppable"
    );
}

#[test]
fn grep_is_case_insensitive_when_requested() {
    let xlsx = shared_string_xlsx();
    let opts = GrepOptions {
        ignore_case: true,
        limit: None,
    };
    let matches = grep("book.xlsx", &xlsx, "northwind", &opts).unwrap();
    assert!(!matches.is_empty(), "ignore_case should match 'Northwind'");
}

#[test]
fn read_row_range_hydrates_resolved_cells() {
    let xlsx = shared_string_xlsx();
    let nodes = read("book.xlsx", &xlsx, &["sheet[0].rows[0-1]".to_string()]).unwrap();

    assert_eq!(nodes.len(), 2, "two rows in range, got {}", nodes.len());

    // Flatten every cell's text across the returned rows.
    let cell_text: Vec<String> = nodes
        .iter()
        .flat_map(|row| row.children.iter())
        .filter_map(|c| c.text.clone())
        .collect();

    assert!(cell_text.iter().any(|t| t == "Region"));
    assert!(cell_text.iter().any(|t| t == "APAC"));
    assert!(cell_text.iter().any(|t| t == "Northwind Trading"));
    assert!(cell_text.iter().any(|t| t == "42"), "numeric cell preserved");
}

#[test]
fn read_inline_string_row_range_hydrates() {
    let xlsx = inline_string_xlsx();
    let nodes = read("book.xlsx", &xlsx, &["sheet[0].rows[0-1]".to_string()]).unwrap();
    let cell_text: Vec<String> = nodes
        .iter()
        .flat_map(|row| row.children.iter())
        .filter_map(|c| c.text.clone())
        .collect();
    assert!(cell_text.iter().any(|t| t == "Northwind Trading"));
    assert!(cell_text.iter().any(|t| t == "99"));
}

#[test]
fn grep_miss_returns_no_matches() {
    let xlsx = shared_string_xlsx();
    let opts = GrepOptions::default();
    let matches = grep("book.xlsx", &xlsx, "NoSuchValueAnywhere", &opts).unwrap();
    assert!(matches.is_empty());
}

/// A workbook with enough rows to span multiple 100-row ranges, with a
/// distinctive value placed exactly on a range boundary.
fn many_rows_xlsx() -> Vec<u8> {
    let mut data = String::new();
    for r in 1..=150 {
        // Row r holds an inline string "valR" in column A.
        data.push_str(&format!(
            "<row r=\"{r}\"><c r=\"A{r}\" t=\"inlineStr\"><is><t>val{r}</t></is></c></row>"
        ));
    }
    let sheet = format!(
        "<?xml version=\"1.0\"?><worksheet xmlns=\"x\"><sheetData>{data}</sheetData></worksheet>"
    );
    build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/worksheets/sheet1.xml", &sheet),
    ])
}

#[test]
fn row_ranges_chunk_and_respect_inclusive_bounds() {
    clear_scan_cache();
    let xlsx = many_rows_xlsx();
    let manifest = scan("book.xlsx", &xlsx).unwrap();

    // 150 rows → ranges 0-99 and 100-149 (plus the sheet block).
    assert!(manifest.blocks.iter().any(|b| b.id == "sheet[0].rows[0-99]"));
    assert!(manifest
        .blocks
        .iter()
        .any(|b| b.id == "sheet[0].rows[100-149]"));

    // The first range hydrates exactly 100 rows (the inclusive end, ordinal 99,
    // must be included — the byte-offset bug dropped every row).
    let first = read("book.xlsx", &xlsx, &["sheet[0].rows[0-99]".to_string()]).unwrap();
    assert_eq!(first.len(), 100);
    // Ordinal 99 is source row 100 ("val100"); it belongs to the first range.
    let last_cell = first
        .last()
        .and_then(|row| row.children.first())
        .and_then(|c| c.text.clone());
    assert_eq!(last_cell.as_deref(), Some("val100"));

    let second = read("book.xlsx", &xlsx, &["sheet[0].rows[100-149]".to_string()]).unwrap();
    assert_eq!(second.len(), 50);

    // A value in the second range is greppable and attributed to that range.
    let matches = grep("book.xlsx", &xlsx, "val150", &GrepOptions::default()).unwrap();
    assert!(matches
        .iter()
        .any(|m| m.block_id == "sheet[0].rows[100-149]"));
}

#[test]
fn read_row_range_ids_from_scan_resolve() {
    // Every row-range id scan advertises must hydrate to at least one row.
    clear_scan_cache();
    let xlsx = shared_string_xlsx();
    let manifest = scan("book.xlsx", &xlsx).unwrap();
    for block in manifest.blocks.iter().filter(|b| b.id.contains(".rows[")) {
        let nodes = read("book.xlsx", &xlsx, std::slice::from_ref(&block.id)).unwrap();
        assert!(
            !nodes.is_empty(),
            "range id {} returned no rows",
            block.id
        );
    }
}
