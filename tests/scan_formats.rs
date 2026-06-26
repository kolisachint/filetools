//! Scan-level tests for the lightweight format handlers (mermaid, csv, html,
//! zip, image metadata). These exercise the manifest-first `scan` API.

use std::io::Write;

use filetools_rs::model::BlockType;
use filetools_rs::{clear_scan_cache, read, scan};

#[test]
fn mermaid_scans_diagram_and_subgraph_blocks() {
    let mmd =
        b"graph TD\n  A[Start] --> B{Decision}\n  subgraph Sub1\n    C --> D\n  end\n  B --> C";
    let result = scan("flow.mmd", mmd).unwrap();

    // Header block plus body/subgraph blocks.
    assert!(result.block_count >= 3);
    let header = &result.blocks[0];
    assert_eq!(header.id, "diagram");
    assert!(header.preview.contains("graph"));
    assert!(result.blocks.iter().any(|b| b.id == "subgraph:Sub1"));
}

#[test]
fn csv_scans_header_and_row_ranges() {
    let mut csv = String::from("name,age,city\n");
    for i in 0..250 {
        csv.push_str(&format!("Person{i},{},City{i}\n", 20 + i % 50));
    }
    let result = scan("data.csv", csv.as_bytes()).unwrap();

    // Header + 3 row-range chunks (100 rows each over 250 rows).
    assert_eq!(result.block_count, 4);
    assert_eq!(result.blocks[0].id, "header");
    assert_eq!(result.blocks[0].block_type, BlockType::Section);
    assert_eq!(result.blocks[1].id, "rows[0-99]");
    assert_eq!(result.blocks[2].id, "rows[100-199]");
    assert_eq!(result.blocks[3].id, "rows[200-249]");
}

#[test]
fn html_scans_title_and_heading_sections() {
    let html = b"<html><head><title>My Page</title></head>\
        <body><h1>Intro</h1><p>hello</p><h2>Details</h2><p>more</p></body></html>";
    let result = scan("page.html", html).unwrap();

    assert_eq!(result.blocks[0].id, "title");
    assert_eq!(result.blocks[0].preview, "My Page");
    assert!(result.blocks.iter().any(|b| b.id == "section[0]"));
    assert!(result.blocks.iter().any(|b| b.id == "section[1]"));
    // Paragraphs are addressable too, so an agent can discover them to edit.
    assert!(result.blocks.iter().any(|b| b.id == "paragraph[0]"));
    assert!(result.blocks.iter().any(|b| b.id == "paragraph[1]"));
}

#[test]
fn generic_xml_scan_ids_resolve_via_read() {
    // Multiple sibling paragraphs exercise the structural-path counters that
    // the old index_nodes() reset to zero, producing duplicate/garbage ids.
    let xml = br#"<doc><p>First paragraph here.</p><p>Second paragraph here.</p><p>Third paragraph here.</p></doc>"#;
    let manifest = scan("doc.xml", xml).unwrap();
    let ids: Vec<String> = manifest.blocks.iter().map(|b| b.id.clone()).collect();

    // ids must be unique
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "scan produced duplicate ids: {ids:?}"
    );

    // every scan id resolves to a node via read
    for id in &ids {
        let nodes = read("doc.xml", xml, std::slice::from_ref(id)).unwrap();
        assert_eq!(
            nodes.len(),
            1,
            "id {id} did not resolve to exactly one node"
        );
    }
}

#[test]
fn zip_scans_one_block_per_entry() {
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        zip.start_file("a.txt", opts).unwrap();
        zip.write_all(b"hello world").unwrap();
        zip.start_file("dir/b.txt", opts).unwrap();
        zip.write_all(b"second file").unwrap();
        zip.finish().unwrap();
    }

    let result = scan("archive.zip", &buf).unwrap();
    assert_eq!(result.block_count, 2);
    assert!(result.blocks.iter().any(|b| b.id == "entry:a.txt"));
    assert!(result.blocks.iter().any(|b| b.id == "entry:dir/b.txt"));
}

#[test]
fn multibyte_preview_does_not_panic() {
    // A heading followed by a long line of 2-byte chars: byte index 97 lands
    // mid-character, which would panic a naive `&s[..97]` slice.
    let mut md = String::from("# Title\n");
    for _ in 0..200 {
        md.push('\u{00e9}'); // 'é', 2 bytes each
    }
    md.push('\n');
    let result = scan("unicode.md", md.as_bytes()).unwrap();
    assert!(result.block_count >= 1);
    // Preview is truncated with an ellipsis and stays valid UTF-8.
    assert!(result.blocks[0].preview.ends_with("..."));
}

#[test]
fn scan_and_read_ids_match_for_optimized_formats() {
    let csv = b"name,age\nAlice,30\nBob,25\nCarol,35";
    let manifest = scan("data.csv", csv).unwrap();
    let ids: Vec<String> = manifest.blocks.iter().map(|b| b.id.clone()).collect();

    // Every id reported by scan must resolve via read.
    let nodes = read("data.csv", csv, &ids).unwrap();
    assert_eq!(nodes.len(), ids.len());
    for (node, id) in nodes.iter().zip(&ids) {
        assert_eq!(&node.id, id);
    }
}

#[test]
fn read_empty_ids_returns_all_optimized_blocks() {
    let html = b"<html><head><title>T</title></head><body><h1>A</h1><h2>B</h2></body></html>";
    let manifest = scan("page.html", html).unwrap();
    let nodes = read("page.html", html, &[]).unwrap();
    assert_eq!(nodes.len(), manifest.block_count);
}

#[test]
fn scan_cache_invalidates_on_content_change() {
    clear_scan_cache();
    let v1 = b"name,age\nAlice,30";
    let r1 = scan("same_path.csv", v1).unwrap();
    assert_eq!(r1.block_count, 2); // header + 1 row range

    // Same path, different content with more rows: must not be served stale.
    let mut v2 = String::from("name,age\n");
    for i in 0..150 {
        v2.push_str(&format!("P{i},{i}\n"));
    }
    let r2 = scan("same_path.csv", v2.as_bytes()).unwrap();
    assert_eq!(r2.block_count, 3); // header + 2 row ranges (150 rows)
}

#[test]
fn png_scans_dimensions_from_header() {
    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&640u32.to_be_bytes());
    png.extend_from_slice(&480u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);

    let result = scan("image.png", &png).unwrap();
    assert_eq!(result.block_count, 1);
    assert_eq!(result.blocks[0].id, "metadata");
    assert!(result.blocks[0].preview.contains("640x480"));
}
