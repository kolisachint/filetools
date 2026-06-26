#![allow(dead_code, unused_variables, unused_imports)]
//! Comprehensive benchmark of manifest-first API across all file types.
//! Measures latency and throughput for scan, read, edit, and write operations.

use std::io::Write;
use std::time::{Duration, Instant};

use filetools_rs::patch::{Op, Patch};
use filetools_rs::{clear_scan_cache, extract, read, scan, write};

// ── Constants for building test files ──────────────────────────────────────

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

// ── Timing helper ──────────────────────────────────────────────────────────

#[allow(dead_code)]
struct Timer {
    label: String,
    start: Instant,
}

impl Timer {
    fn new(label: &str) -> Self {
        Timer {
            label: label.to_string(),
            start: Instant::now(),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        println!("  {:?} | {:?}", self.label, elapsed);
    }
}

fn format_duration(d: Duration) -> String {
    if d.as_millis() > 100 {
        format!("{:.1}ms", d.as_secs_f64() * 1000.0)
    } else if d.as_micros() > 100 {
        format!("{:.1}us", d.as_micros())
    } else {
        format!("{}ns", d.as_nanos())
    }
}

// ── DOCX builders ──────────────────────────────────────────────────────────

fn build_docx(document_xml: &str) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", RELS),
            ("word/document.xml", document_xml),
        ] {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    }
    out
}

fn build_docx_small() -> Vec<u8> {
    let doc = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Introduction</w:t></w:r></w:p>
    <w:p><w:r><w:t>This is a small test document with minimal content.</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
    build_docx(doc)
}

fn build_docx_medium() -> Vec<u8> {
    let mut paragraphs = Vec::new();
    for i in 0..50 {
        let style = if i % 10 == 0 {
            r#"<w:pPr><w:pStyle w:val="Heading2"/></w:pPr>"#
        } else {
            ""
        };
        paragraphs.push(format!(
            r#"<w:p>{}<w:r><w:t>Paragraph {} with some sample text for benchmarking purposes.</w:t></w:r></w:p>"#,
            style, i
        ));
    }
    // Add a table
    paragraphs.push(r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Col1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Col2</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>Val1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Val2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string());

    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>{}</w:body>
</w:document>"#,
        paragraphs.join("\n    ")
    );
    build_docx(&doc)
}

fn build_docx_large() -> Vec<u8> {
    let mut paragraphs = Vec::new();
    for i in 0..200 {
        let style = if i % 20 == 0 {
            r#"<w:pPr><w:pStyle w:val="Heading1"/></w:pPr>"#
        } else if i % 10 == 0 {
            r#"<w:pPr><w:pStyle w:val="Heading2"/></w:pPr>"#
        } else {
            ""
        };
        paragraphs.push(format!(
            r#"<w:p>{}<w:r><w:t>Section {}, Paragraph {}. This is longer sample text that simulates a real document with multiple sentences. The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet, consectetur adipiscing elit.</w:t></w:r></w:p>"#,
            style, i / 10, i % 10
        ));
    }
    // Add multiple tables
    for _t in 0..5 {
        let mut rows = Vec::new();
        for r in 0..10 {
            rows.push(format!(
                r#"<w:tr><w:tc><w:p><w:r><w:t>R{}C1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>R{}C2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>R{}C3</w:t></w:r></w:p></w:tc></w:tr>"#,
                r, r, r
            ));
        }
        paragraphs.push(format!(r#"<w:tbl>{}</w:tbl>"#, rows.join("")));
    }

    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>{}</w:body>
</w:document>"#,
        paragraphs.join("\n    ")
    );
    build_docx(&doc)
}

// ── XLSX builder ───────────────────────────────────────────────────────────

fn build_xlsx(rows: usize, cols: usize) -> Vec<u8> {
    let mut cells = Vec::new();
    let mut shared_strings = Vec::new();

    for r in 0..rows {
        let mut row_cells = Vec::new();
        for c in 0..cols {
            let text = format!("Cell_{}_{}", r, c);
            shared_strings.push(format!(r#"<si><t>{}</t></si>"#, text));
            row_cells.push(format!(
                r#"<c r="{}{}" t="s"><v>{}</v></c>"#,
                (b'A' + c as u8) as char,
                r + 1,
                r * cols + c
            ));
        }
        cells.push(format!("<row r=\"{}\">{}</row>", r + 1, row_cells.join("")));
    }

    let shared = format!(
        r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{}" uniqueCount="{}">{}</sst>"#,
        rows * cols,
        rows * cols,
        shared_strings.join("")
    );

    let sheet = format!(
        r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{}</sheetData></worksheet>"#,
        cells.join("")
    );

    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;

    let mut out = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", RELS),
            ("xl/workbook.xml", workbook),
            ("xl/sharedStrings.xml", &shared),
            ("xl/worksheets/sheet1.xml", &sheet),
        ] {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
    }
    out
}

// ── PPTX builder ───────────────────────────────────────────────────────────

fn build_pptx(slides: usize) -> Vec<u8> {
    let mut slide_parts = Vec::new();
    let mut rels_parts = Vec::new();

    for i in 0..slides {
        let slide = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:txBody><a:p><a:r><a:t>Slide {} Title</a:t></a:r></a:p></p:txBody></p:sp>
    <p:sp><p:txBody><a:p><a:r><a:t>Slide {} Content with some details.</a:t></a:r></a:p></p:txBody></p:sp>
  </p:spTree></p:cSld>
</p:sld>"#,
            i + 1,
            i + 1
        );
        slide_parts.push((format!("ppt/slides/slide{}.xml", i + 1), slide));

        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
</Relationships>"#;
        rels_parts.push((format!("ppt/slides/_rels/slide{}.xml.rels", i + 1), rels));
    }

    let mut out = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        zw.start_file("[Content_Types].xml", opts).unwrap();
        zw.write_all(CONTENT_TYPES.as_bytes()).unwrap();

        zw.start_file("_rels/.rels", opts).unwrap();
        zw.write_all(RELS.as_bytes()).unwrap();

        for (name, body) in &slide_parts {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }
        for (name, body) in &rels_parts {
            zw.start_file(name, opts).unwrap();
            zw.write_all(body.as_bytes()).unwrap();
        }

        zw.finish().unwrap();
    }
    out
}

// ── XML builder ────────────────────────────────────────────────────────────

fn build_xml(elements: usize) -> Vec<u8> {
    let mut items = Vec::new();
    for i in 0..elements {
        items.push(format!(
            r#"<item id="{}" type="data"><name>Element {}</name><value>Value for item {}</value></item>"#,
            i, i, i
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<root>{}</root>"#,
        items.join("\n  ")
    )
    .into_bytes()
}

// ── Drawio builder ─────────────────────────────────────────────────────────

fn build_drawio(cells: usize) -> Vec<u8> {
    let mut mx_cells = Vec::new();
    mx_cells.push(r#"<mxCell id="0"/>"#.to_string());
    mx_cells.push(r#"<mxCell id="1" parent="0"/>"#.to_string());

    for i in 2..cells + 2 {
        let x = (i - 2) * 150;
        mx_cells.push(format!(
            r#"<mxCell id="{}" value="Node {}" vertex="1" parent="1"><mxGeometry x="{}" y="100" width="120" height="60" as="geometry"/></mxCell>"#,
            i, i - 2, x
        ));
    }

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<mxfile>
  <diagram id="d1" name="Page-1">
    <mxGraphModel>
      <root>{}</root>
    </mxGraphModel>
  </diagram>
</mxfile>"#,
        mx_cells.join("\n    ")
    );
    xml.into_bytes()
}

// ── PDF builder (minimal) ──────────────────────────────────────────────────

fn build_pdf_simple() -> Vec<u8> {
    // Use lopdf to build a minimal PDF
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
    });
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
    ];

    let mut y = 700;
    for i in 0..20 {
        ops.push(Operation::new("Td", vec![72.into(), y.into()]));
        let text = format!("Line {}: Sample text for PDF benchmark", i);
        ops.push(Operation::new(
            "Tj",
            vec![Object::string_literal(text.as_bytes())],
        ));
        y -= 30;
    }
    ops.push(Operation::new("ET", vec![]));

    let content = Content { operations: ops };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
    });
    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

// ── Benchmark runner ───────────────────────────────────────────────────────

struct BenchResult {
    name: String,
    size: usize,
    scan_time: Duration,
    read_time: Duration,
    edit_time: Duration,
    write_time: Duration,
    block_count: usize,
    token_count: usize,
}

fn run_bench(name: &str, data: &[u8], path: &str) -> BenchResult {
    println!("\n--- {} ({} bytes) ---", name, data.len());

    clear_scan_cache();

    // Scan
    let start = Instant::now();
    let scan_result = scan(path, data).unwrap();
    let scan_time = start.elapsed();
    println!(
        "  scan:  {:?} | {} blocks, {} tokens",
        scan_time, scan_result.block_count, scan_result.total_tokens
    );

    // Read (first 5 blocks)
    let ids: Vec<String> = scan_result
        .blocks
        .iter()
        .take(5)
        .map(|b| b.id.clone())
        .collect();
    let start = Instant::now();
    let blocks = read(path, data, &ids).unwrap();
    let read_time = start.elapsed();
    println!(
        "  read:  {:?} | {} blocks returned",
        read_time,
        blocks.len()
    );

    // Extract for editing
    let extract_out = extract(path, data).unwrap();
    let envelope = &extract_out.envelope;
    let idmap = extract_out.idmap.as_ref().unwrap();

    // Find a text block to edit
    let edit_id = find_editable_text(envelope);
    let edit_time = if let Some(id) = edit_id {
        let guard = idmap.get(&id).map(|l| l.hash.clone());
        let patch = Patch {
            patch: vec![
                Op::Test {
                    path: format!("/structure/{id}"),
                    hash: guard.unwrap(),
                },
                Op::Replace {
                    path: format!("/structure/{id}/text"),
                    value: "BENCHMARK EDIT APPLIED".to_string(),
                },
            ],
        };
        let start = Instant::now();
        let _ = write(envelope, idmap, data, &patch).unwrap();
        start.elapsed()
    } else {
        Duration::ZERO
    };
    println!("  edit:  {:?}", edit_time);

    // Write (reconstruct with empty patch)
    let patch = Patch { patch: vec![] };
    let start = Instant::now();
    let _ = write(envelope, idmap, data, &patch).unwrap();
    let write_time = start.elapsed();
    println!("  write: {:?}", write_time);

    BenchResult {
        name: name.to_string(),
        size: data.len(),
        scan_time,
        read_time,
        edit_time,
        write_time,
        block_count: scan_result.block_count,
        token_count: scan_result.total_tokens,
    }
}

fn find_editable_text(env: &filetools_rs::model::Envelope) -> Option<String> {
    fn walk(nodes: &[filetools_rs::model::DocNode]) -> Option<String> {
        for n in nodes {
            if let Some(text) = &n.text {
                if text.len() > 10 && !text.starts_with("BENCHMARK") {
                    return Some(n.id.clone());
                }
            }
            if let Some(found) = walk(&n.children) {
                return Some(found);
            }
        }
        None
    }
    walk(&env.structure)
}

// ── Main ───────────────────────────────────────────────────────────────────

fn main() {
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║           Manifest-First API Benchmark Suite                   ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");

    let mut results = Vec::new();

    // ── DOCX tests ─────────────────────────────────────────────────────
    println!("\n━━━ DOCX Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let docx_small = build_docx_small();
    results.push(run_bench(
        "DOCX Small (2 paragraphs)",
        &docx_small,
        "test.docx",
    ));

    let docx_medium = build_docx_medium();
    results.push(run_bench(
        "DOCX Medium (50 paragraphs + table)",
        &docx_medium,
        "test.docx",
    ));

    let docx_large = build_docx_large();
    results.push(run_bench(
        "DOCX Large (200 paragraphs + 5 tables)",
        &docx_large,
        "test.docx",
    ));

    // ── XLSX tests ─────────────────────────────────────────────────────
    println!("\n━━━ XLSX Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let xlsx_small = build_xlsx(10, 5);
    results.push(run_bench("XLSX Small (10x5)", &xlsx_small, "test.xlsx"));

    let xlsx_medium = build_xlsx(50, 10);
    results.push(run_bench("XLSX Medium (50x10)", &xlsx_medium, "test.xlsx"));

    let xlsx_large = build_xlsx(100, 20);
    results.push(run_bench("XLSX Large (100x20)", &xlsx_large, "test.xlsx"));

    // ── PPTX tests ━────────────────────────────────────────────────────
    println!("\n━━━ PPTX Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let pptx_small = build_pptx(3);
    results.push(run_bench("PPTX Small (3 slides)", &pptx_small, "test.pptx"));

    let pptx_medium = build_pptx(10);
    results.push(run_bench(
        "PPTX Medium (10 slides)",
        &pptx_medium,
        "test.pptx",
    ));

    let pptx_large = build_pptx(25);
    results.push(run_bench(
        "PPTX Large (25 slides)",
        &pptx_large,
        "test.pptx",
    ));

    // ── XML tests ──────────────────────────────────────────────────────
    println!("\n━━━ XML Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let xml_small = build_xml(10);
    results.push(run_bench("XML Small (10 elements)", &xml_small, "test.xml"));

    let xml_medium = build_xml(100);
    results.push(run_bench(
        "XML Medium (100 elements)",
        &xml_medium,
        "test.xml",
    ));

    let xml_large = build_xml(500);
    results.push(run_bench(
        "XML Large (500 elements)",
        &xml_large,
        "test.xml",
    ));

    // ── Drawio tests ───────────────────────────────────────────────────
    println!("\n━━━ Drawio Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let drawio_small = build_drawio(5);
    results.push(run_bench(
        "Drawio Small (5 cells)",
        &drawio_small,
        "test.drawio",
    ));

    let drawio_medium = build_drawio(20);
    results.push(run_bench(
        "Drawio Medium (20 cells)",
        &drawio_medium,
        "test.drawio",
    ));

    let drawio_large = build_drawio(50);
    results.push(run_bench(
        "Drawio Large (50 cells)",
        &drawio_large,
        "test.drawio",
    ));

    // ── PDF tests ──────────────────────────────────────────────────────
    println!("\n━━━ PDF Tests ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let pdf = build_pdf_simple();
    results.push(run_bench("PDF Simple (20 lines)", &pdf, "test.pdf"));

    // ── Summary ────────────────────────────────────────────────────────
    println!("\n\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║                         Summary                                ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    println!(
        "{:<40} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Test", "Size", "Blocks", "Tokens", "Scan", "Read", "Write"
    );
    println!("{}", "─".repeat(100));

    for r in &results {
        println!(
            "{:<40} {:>7}B {:>8} {:>8} {:>8} {:>8} {:>8}",
            r.name,
            r.size,
            r.block_count,
            r.token_count,
            format_duration(r.scan_time),
            format_duration(r.read_time),
            format_duration(r.write_time),
        );
    }

    // Throughput calculation
    println!("\n\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║                      Throughput (MB/s)                         ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    println!(
        "{:<40} {:>10} {:>10} {:>10}",
        "Test", "Scan", "Read", "Write"
    );
    println!("{}", "─".repeat(75));

    for r in &results {
        let size_mb = r.size as f64 / (1024.0 * 1024.0);
        let scan_throughput = if r.scan_time.as_secs_f64() > 0.0 {
            size_mb / r.scan_time.as_secs_f64()
        } else {
            0.0
        };
        let read_throughput = if r.read_time.as_secs_f64() > 0.0 {
            size_mb / r.read_time.as_secs_f64()
        } else {
            0.0
        };
        let write_throughput = if r.write_time.as_secs_f64() > 0.0 {
            size_mb / r.write_time.as_secs_f64()
        } else {
            0.0
        };

        println!(
            "{:<40} {:>9.1} {:>9.1} {:>9.1}",
            r.name, scan_throughput, read_throughput, write_throughput,
        );
    }

    println!("\n✓ Benchmark complete");
}
