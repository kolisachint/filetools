//! Profile bottlenecks for 3-15MB files, focusing on XLSX.

use std::io::Write;
use std::time::{Duration, Instant};

use filetools_rs::patch::Patch;
use filetools_rs::{clear_scan_cache, extract, read, scan, write};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

fn build_xlsx(rows: usize, cols: usize) -> Vec<u8> {
    let mut cells = Vec::new();
    let mut shared_strings = Vec::new();

    for r in 0..rows {
        let mut row_cells = Vec::new();
        for c in 0..cols {
            let text = format!("Cell_{}_{}_{}", r, c, "x".repeat(20)); // Longer text
            shared_strings.push(format!("<si><t>{}</t></si>", text));
            row_cells.push(format!(
                r#"<c r="{}{}" t="s"><v>{}</v></c>"#,
                (b'A' + (c % 26) as u8) as char,
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

struct ProfileResult {
    name: String,
    size: usize,
    extract_time: Duration,
    scan_time: Duration,
    read_time: Duration,
    write_time: Duration,
    block_count: usize,
    idmap_entries: usize,
}

fn profile(name: &str, data: &[u8]) -> ProfileResult {
    println!(
        "\n--- {} ({:.1} KB, {} rows x {} cols) ---",
        name,
        data.len() as f64 / 1024.0,
        0,
        0
    );

    clear_scan_cache();

    // Profile extract (full parse)
    let start = Instant::now();
    let extract_out = extract("test.xlsx", data).unwrap();
    let extract_time = start.elapsed();
    let block_count = extract_out.envelope.structure.len();
    let idmap_entries = extract_out.idmap.as_ref().map(|m| m.map.len()).unwrap_or(0);
    println!(
        "  extract:  {:?} ({} blocks, {} idmap entries)",
        extract_time, block_count, idmap_entries
    );

    // Profile scan (manifest only)
    clear_scan_cache();
    let start = Instant::now();
    let scan_result = scan("test.xlsx", data).unwrap();
    let scan_time = start.elapsed();
    println!(
        "  scan:     {:?} ({} blocks, {} tokens)",
        scan_time, scan_result.block_count, scan_result.total_tokens
    );

    // Profile read (selective)
    let ids: Vec<String> = scan_result
        .blocks
        .iter()
        .filter(|b| b.block_type == filetools_rs::model::BlockType::Cell)
        .take(10)
        .map(|b| b.id.clone())
        .collect();

    let start = Instant::now();
    let blocks = read("test.xlsx", data, &ids).unwrap();
    let read_time = start.elapsed();
    println!(
        "  read(10): {:?} ({} blocks returned)",
        read_time,
        blocks.len()
    );

    // Profile write (empty patch)
    let patch = Patch { patch: vec![] };
    let start = Instant::now();
    let _ = write(
        &extract_out.envelope,
        extract_out.idmap.as_ref().unwrap(),
        data,
        &patch,
    )
    .unwrap();
    let write_time = start.elapsed();
    println!("  write:    {:?}", write_time);

    ProfileResult {
        name: name.to_string(),
        size: data.len(),
        extract_time,
        scan_time,
        read_time,
        write_time,
        block_count: scan_result.block_count,
        idmap_entries,
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║           Profile: 3-15MB File Performance                     ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");

    let mut results = Vec::new();

    // XLSX sizes targeting 3-15MB
    let configs = vec![
        ("XLSX 200x50", 200, 50),
        ("XLSX 500x100", 500, 100),
        ("XLSX 1000x150", 1000, 150),
        ("XLSX 2000x200", 2000, 200),
    ];

    for (name, rows, cols) in configs {
        let data = build_xlsx(rows, cols);
        let size_mb = data.len() as f64 / (1024.0 * 1024.0);

        if (2.0..=16.0).contains(&size_mb) {
            println!("\n{:=>60}", format!(" {:.1}MB ", size_mb));
            results.push(profile(&format!("{} ({:.1}MB)", name, size_mb), &data));
        } else {
            println!(
                "\nSkipping {} ({:.1}MB) - outside target range",
                name, size_mb
            );
        }
    }

    // Summary
    println!("\n\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║                         Summary                                ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    println!(
        "{:<35} {:>8} {:>10} {:>10} {:>10} {:>10}",
        "Test", "Size", "Extract", "Scan", "Read(10)", "Write"
    );
    println!("{}", "─".repeat(90));

    for r in &results {
        println!(
            "{:<35} {:>7.1}MB {:>10} {:>10} {:>10} {:>10}",
            r.name,
            r.size as f64 / (1024.0 * 1024.0),
            format!("{:.1?}", r.extract_time),
            format!("{:.1?}", r.scan_time),
            format!("{:.1?}", r.read_time),
            format!("{:.1?}", r.write_time),
        );
    }

    // Analysis
    println!("\n\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║                      Analysis                                  ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");

    if let Some(last) = results.last() {
        println!(
            "\nLargest file: {} ({:.1}MB)",
            last.name,
            last.size as f64 / (1024.0 * 1024.0)
        );
        println!("  Blocks: {}", last.block_count);
        println!("  IdMap entries: {}", last.idmap_entries);
        println!(
            "  Avg time per block (scan): {:.2}µs",
            last.scan_time.as_micros() as f64 / last.block_count as f64
        );
        println!(
            "  Avg time per idmap entry (extract): {:.2}µs",
            last.extract_time.as_micros() as f64 / last.idmap_entries as f64
        );
    }

    // Bottleneck identification
    println!("\nIdentified bottlenecks:");
    println!("  1. XLSX creates a block per cell → high block count");
    println!("  2. IdMap stores per-cell entries → large memory footprint");
    println!("  3. extract() and scan() both parse full file → no shared work");
    println!("  4. read() re-parses full file → redundant I/O");

    println!("\nSuggested optimizations:");
    println!("  1. Hierarchical XLSX blocks: sheet → row → cell (reduce block count)");
    println!("  2. Lazy idmap: only populate entries for accessed cells");
    println!("  3. Shared parsing cache: extract() result cached for scan()/read()");
    println!("  4. Streaming XML parser: avoid loading entire sheet into memory");
    println!("  5. Cell range batching: read/edit cell ranges instead of individual cells");
}
