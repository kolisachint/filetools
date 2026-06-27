//! Discovery-loop tests for pptx: scan previews, grep, and slide read must
//! reach a slide's *full* text — including runs past the preview cutoff, runs
//! that carry attributes (`<a:t xml:space="preserve">`), and entity-encoded
//! text — not just the truncated scan preview.

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
const PRESENTATION: &str = r#"<?xml version="1.0"?><presentation/>"#;

/// Wrap text runs into a slide part, one `<a:p>` paragraph per run.
fn slide(runs: &[&str]) -> String {
    let body: String = runs
        .iter()
        .map(|r| format!("<a:p><a:r>{r}</a:r></a:p>"))
        .collect();
    format!(
        r#"<?xml version="1.0"?><p:sld xmlns:a="a" xmlns:p="p"><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>"#
    )
}

/// A two-slide deck: slide 1 is short; slide 2 has filler text, a distinctive
/// word past the ~100-char preview cutoff, a run carrying attributes, and an
/// entity-encoded run.
fn deck() -> Vec<u8> {
    let filler = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor";
    let s1 = slide(&["<a:t>Hello World</a:t>"]);
    let s2 = slide(&[
        &format!("<a:t>{filler}</a:t>"),
        "<a:t>DistinctiveDeepWord</a:t>",
        r#"<a:t xml:space="preserve">PreserveRun</a:t>"#,
        "<a:t>Tom &amp; Jerry</a:t>",
    ]);
    build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("ppt/presentation.xml", PRESENTATION),
        ("ppt/slides/slide1.xml", &s1),
        ("ppt/slides/slide2.xml", &s2),
    ])
}

#[test]
fn scan_reports_one_block_per_slide() {
    clear_scan_cache();
    let pptx = deck();
    let result = scan("deck.pptx", &pptx).unwrap();
    assert_eq!(result.block_count, 2);
    assert_eq!(result.blocks[0].id, "slide[0]");
    assert_eq!(result.blocks[1].id, "slide[1]");
    assert!(result.blocks[0].preview.contains("Hello World"));
}

#[test]
fn grep_reaches_text_past_the_preview_cutoff() {
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "DistinctiveDeepWord", &GrepOptions::default()).unwrap();
    assert!(matches.iter().any(|m| m.block_id == "slide[1]"));
    assert!(matches.iter().all(|m| m.writable), "pptx is editable");
}

#[test]
fn grep_matches_runs_with_attributes() {
    // `<a:t xml:space="preserve">` runs were skipped entirely by the old
    // `<a:t>`-exact scan — text and run count both missed them.
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "PreserveRun", &GrepOptions::default()).unwrap();
    assert!(
        matches.iter().any(|m| m.block_id == "slide[1]"),
        "attributed run text must be greppable"
    );
}

#[test]
fn grep_matches_entity_decoded_text() {
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "Tom & Jerry", &GrepOptions::default()).unwrap();
    assert!(
        matches.iter().any(|m| m.block_id == "slide[1]"),
        "entity-encoded run text must be unescaped before matching"
    );
}

#[test]
fn grep_is_case_insensitive_when_requested() {
    let pptx = deck();
    let opts = GrepOptions {
        ignore_case: true,
        limit: None,
    };
    let matches = grep("deck.pptx", &pptx, "distinctivedeepword", &opts).unwrap();
    assert!(matches.iter().any(|m| m.block_id == "slide[1]"));
}

#[test]
fn grep_does_not_match_synthetic_slide_prefix() {
    // The synthetic "Slide N:" preview must not be searched, or the literal
    // word would match every slide.
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "Slide 2:", &GrepOptions::default()).unwrap();
    assert!(matches.is_empty());
}

#[test]
fn read_slide_hydrates_full_paragraph_text() {
    let pptx = deck();
    let nodes = read("deck.pptx", &pptx, &["slide[1]".to_string()]).unwrap();
    assert_eq!(nodes.len(), 1);
    let slide = &nodes[0];
    assert_eq!(slide.attrs.iter().find(|a| a.name == "runs").unwrap().value, "4");

    let para_text: Vec<String> = slide
        .children
        .iter()
        .filter_map(|c| c.text.clone())
        .collect();
    assert!(para_text.iter().any(|t| t == "DistinctiveDeepWord"));
    assert!(para_text.iter().any(|t| t == "PreserveRun"));
    assert!(para_text.iter().any(|t| t == "Tom & Jerry"));
}

#[test]
fn grep_miss_returns_no_matches() {
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "NoSuchWordAnywhere", &GrepOptions::default()).unwrap();
    assert!(matches.is_empty());
}

#[test]
fn grep_slide_ids_resolve_via_read() {
    // Every block id grep reports must hydrate via read.
    let pptx = deck();
    let matches = grep("deck.pptx", &pptx, "World", &GrepOptions::default()).unwrap();
    assert!(!matches.is_empty());
    for m in &matches {
        let nodes = read("deck.pptx", &pptx, std::slice::from_ref(&m.block_id)).unwrap();
        assert_eq!(nodes.len(), 1, "grep id {} did not resolve", m.block_id);
    }
}
