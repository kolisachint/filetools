//! End-to-end demo of the manifest-first API workflow.

use std::io::{Read, Write};

use filetools_rs::patch::{Op, Patch};
use filetools_rs::{clear_scan_cache, extract, read, scan, write};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

/// Build a minimal docx with headings and paragraphs.
fn build_test_docx() -> Vec<u8> {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Quarterly Report</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>This is the introduction paragraph with some initial context.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
      <w:r><w:t>Revenue Analysis</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Revenue increased by 15% compared to last quarter.</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Q1</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Q2</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>North</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>$1.2M</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>$1.5M</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>South</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>$0.8M</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>$1.1M</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
      <w:r><w:t>Conclusion</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>All regions showed positive growth this quarter.</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

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

fn print_nodes(nodes: &[filetools_rs::model::DocNode], depth: usize) {
    let indent = "  ".repeat(depth);
    for node in nodes {
        println!(
            "{}<{}> id={} text={:?} attrs={:?}",
            indent, node.tag, node.id, node.text, node.attrs
        );
        if !node.children.is_empty() {
            print_nodes(&node.children, depth + 1);
        }
    }
}

fn main() {
    println!("=== Manifest-First API Demo ===\n");

    // Build test document
    let docx = build_test_docx();
    println!("Built test DOCX: {} bytes\n", docx.len());

    // Step 1: Scan - get lightweight manifest
    println!("--- Step 1: Scan ---");
    clear_scan_cache();
    let scan_result = scan("report.docx", &docx).unwrap();
    println!("File type: {:?}", scan_result.file_type);
    println!("Block count: {}", scan_result.block_count);
    println!("Total tokens: {}", scan_result.total_tokens);

    // Debug: print full structure
    println!("\n--- Debug: Full Structure ---");
    let extract_out = extract("report.docx", &docx).unwrap();
    print_nodes(&extract_out.envelope.structure, 0);

    println!("\nBlocks:");
    for block in &scan_result.blocks {
        println!(
            "  {:?} | id={:?} | parent={:?} | preview={:?}",
            block.block_type, block.id, block.parent_id, block.preview
        );
    }

    // Step 2: Read - fetch specific blocks
    println!("\n--- Step 2: Read (selective) ---");
    let target_ids = vec![
        "heading[0]".to_string(),
        "paragraph[0]".to_string(),
        "table[0].row[0].cell[0]".to_string(),
    ];
    let blocks = read("report.docx", &docx, &target_ids).unwrap();
    println!("Read {} blocks:", blocks.len());
    for block in &blocks {
        println!(
            "  tag={:?} text={:?}",
            block.tag,
            block.text.as_deref().unwrap_or("<no text>")
        );
    }

    // Step 3: Extract for editing (need envelope + idmap)
    println!("\n--- Step 3: Extract for editing ---");
    let extract_out = extract("report.docx", &docx).unwrap();
    let envelope = &extract_out.envelope;
    let idmap = extract_out.idmap.as_ref().unwrap();
    println!("Envelope writable: {}", envelope.writable);
    println!("IdMap entries: {}", idmap.map.len());

    // Find a block to edit (the revenue paragraph)
    let revenue_text_id = find_text_id(envelope, "Revenue increased by 15%");
    println!("\nFound revenue paragraph: {:?}", revenue_text_id);

    if let Some(id) = &revenue_text_id {
        let guard = idmap.get(id).map(|loc| loc.hash.clone());

        // Step 4: Edit - apply patch with guard
        println!("\n--- Step 4: Edit ---");
        let patch = Patch {
            patch: vec![
                Op::Test {
                    path: format!("/structure/{id}"),
                    hash: guard.unwrap(),
                },
                Op::Replace {
                    path: format!("/structure/{id}/text"),
                    value: "Revenue increased by 20% compared to last quarter.".to_string(),
                },
            ],
        };

        let new_docx = write(envelope, idmap, &docx, &patch).unwrap();
        println!("Reconstructed DOCX: {} bytes", new_docx.len());

        // Verify the edit
        let doc = read_part(&new_docx, "word/document.xml");
        if doc.contains("20%") {
            println!("Edit verified: text now contains '20%'");
        } else {
            println!("ERROR: edit not applied correctly");
        }

        // Step 5: Re-scan to show updated manifest
        println!("\n--- Step 5: Re-scan ---");
        clear_scan_cache();
        let new_scan = scan("report.docx", &new_docx).unwrap();
        let new_block = new_scan.blocks.iter().find(|b| b.preview.contains("20%"));
        if let Some(block) = new_block {
            println!("Updated block found:");
            println!("  id={:?}", block.id);
            println!("  preview={:?}", block.preview);
        }
    }

    println!("\n=== Demo Complete ===");
}

/// Find a block ID by searching text content.
fn find_text_id(envelope: &filetools_rs::model::Envelope, search: &str) -> Option<String> {
    fn walk(nodes: &[filetools_rs::model::DocNode], search: &str) -> Option<String> {
        for n in nodes {
            if let Some(text) = &n.text {
                if text.contains(search) {
                    return Some(n.id.clone());
                }
            }
            if let Some(found) = walk(&n.children, search) {
                return Some(found);
            }
        }
        None
    }
    walk(&envelope.structure, search)
}

/// Read a part from a docx zip.
fn read_part(container: &[u8], name: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(container)).unwrap();
    let mut f = zip.by_name(name).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}
