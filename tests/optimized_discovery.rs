//! Discovery-loop tests for the remaining optimized formats (csv, markdown,
//! html, mermaid, drawio). Each previously surfaced only a truncated scan
//! preview, so grep/read could not reach content past the cutoff (or, for csv,
//! past the first row of a chunk; for drawio, cell labels at all). These assert
//! the full content is now greppable and that every grep id resolves via read.

use filetools_rs::model::GrepOptions;
use filetools_rs::{grep, read};

/// grep for `needle`, returning the matched block ids.
fn grep_ids(path: &str, bytes: &[u8], needle: &str) -> Vec<String> {
    grep(path, bytes, needle, &GrepOptions::default())
        .unwrap()
        .into_iter()
        .map(|m| m.block_id)
        .collect()
}

/// Assert every id grep reports for `needle` resolves to a node via read.
fn assert_grep_ids_resolve(path: &str, bytes: &[u8], needle: &str) {
    let ids = grep_ids(path, bytes, needle);
    assert!(!ids.is_empty(), "expected a match for {needle:?} in {path}");
    for id in &ids {
        let nodes = read(path, bytes, std::slice::from_ref(id)).unwrap();
        assert!(!nodes.is_empty(), "grep id {id} did not resolve via read");
    }
}

// ── CSV ─────────────────────────────────────────────────────────────────────

fn big_csv() -> Vec<u8> {
    let mut s = String::from("name,note\n");
    for i in 0..120 {
        s.push_str(&format!("P{i},note{i}\n"));
    }
    // Value in a non-first row of the first 100-row chunk.
    s.push_str("Px,DEEPCSVWORD\n");
    s.into_bytes()
}

#[test]
fn csv_grep_reaches_non_first_rows() {
    let csv = big_csv();
    // Row 50 (mid first chunk) and the deep marker (second chunk) both match.
    assert!(grep_ids("data.csv", &csv, "note50").contains(&"rows[0-99]".to_string()));
    assert!(grep_ids("data.csv", &csv, "DEEPCSVWORD").contains(&"rows[100-120]".to_string()));
}

#[test]
fn csv_read_range_hydrates_all_rows() {
    let csv = big_csv();
    let nodes = read("data.csv", &csv, &["rows[0-99]".to_string()]).unwrap();
    assert_eq!(nodes.len(), 1, "read returns the range block");
    assert_eq!(nodes[0].id, "rows[0-99]");
    assert_eq!(nodes[0].children.len(), 100, "all 100 rows hydrated");
}

#[test]
fn csv_grep_ids_resolve() {
    let csv = big_csv();
    assert_grep_ids_resolve("data.csv", &csv, "DEEPCSVWORD");
}

// ── Markdown ────────────────────────────────────────────────────────────────

fn big_md() -> Vec<u8> {
    let filler = "filler words here ".repeat(8); // > 100 chars before the marker
    format!("# Sec1\n{filler}DEEPMDWORD tail\n\n# Sec2\nshort body\n").into_bytes()
}

#[test]
fn markdown_grep_reaches_past_preview_in_non_final_section() {
    let md = big_md();
    // The marker sits past the ~100-char preview of a *non-final* section.
    assert!(grep_ids("doc.md", &md, "DEEPMDWORD").contains(&"section[0]".to_string()));
}

#[test]
fn markdown_grep_finds_heading_and_resolves() {
    let md = big_md();
    assert!(grep_ids("doc.md", &md, "Sec2").contains(&"section[1]".to_string()));
    assert_grep_ids_resolve("doc.md", &md, "DEEPMDWORD");
}

// ── HTML ────────────────────────────────────────────────────────────────────

fn big_html() -> Vec<u8> {
    let body = "word ".repeat(30); // > 100 chars before the marker
    format!("<html><body><h1>Title</h1><p>{body}DEEPHTMLWORD end</p></body></html>").into_bytes()
}

#[test]
fn html_grep_reaches_past_paragraph_preview() {
    let html = big_html();
    assert!(grep_ids("page.html", &html, "DEEPHTMLWORD").contains(&"paragraph[0]".to_string()));
}

#[test]
fn html_grep_ids_resolve() {
    let html = big_html();
    assert_grep_ids_resolve("page.html", &html, "DEEPHTMLWORD");
}

// ── Mermaid ─────────────────────────────────────────────────────────────────

fn big_mmd() -> Vec<u8> {
    let mut lines = vec!["graph TD".to_string()];
    for i in 0..20 {
        lines.push(format!("  N{i}[Node number {i} label] --> N{}[Next {i}]", i + 1));
    }
    lines.push("  Z[DEEPMMDWORD]".to_string());
    (lines.join("\n") + "\n").into_bytes()
}

#[test]
fn mermaid_grep_reaches_statements_past_preview() {
    let mmd = big_mmd();
    let ids = grep_ids("flow.mmd", &mmd, "DEEPMMDWORD");
    assert!(!ids.is_empty(), "deep mermaid statement must be greppable");
    assert_grep_ids_resolve("flow.mmd", &mmd, "DEEPMMDWORD");
}

// ── drawio ──────────────────────────────────────────────────────────────────

fn drawio_with_label(label: &str) -> Vec<u8> {
    format!(
        r#"<mxfile><diagram id="d1" name="Page-1"><mxGraphModel><root><mxCell id="2" value="{label}" vertex="1"/></root></mxGraphModel></diagram></mxfile>"#
    )
    .into_bytes()
}

#[test]
fn drawio_grep_finds_cell_label_in_value_attr() {
    // drawio stores labels in the `value` attribute, not element text.
    let dio = drawio_with_label("DEEPDRAWIOWORD");
    let ids = grep_ids("flow.drawio", &dio, "DEEPDRAWIOWORD");
    assert!(!ids.is_empty(), "cell label must be greppable");
    assert_grep_ids_resolve("flow.drawio", &dio, "DEEPDRAWIOWORD");
}

// ── Cross-cutting ───────────────────────────────────────────────────────────

#[test]
fn grep_miss_returns_no_matches_across_formats() {
    assert!(grep_ids("data.csv", &big_csv(), "NoSuchValueXYZ").is_empty());
    assert!(grep_ids("doc.md", &big_md(), "NoSuchValueXYZ").is_empty());
    assert!(grep_ids("page.html", &big_html(), "NoSuchValueXYZ").is_empty());
    assert!(grep_ids("flow.mmd", &big_mmd(), "NoSuchValueXYZ").is_empty());
    assert!(grep_ids("flow.drawio", &drawio_with_label("Lbl"), "NoSuchValueXYZ").is_empty());
}

#[test]
fn grep_limit_is_honored() {
    let csv = big_csv();
    // "note" appears in every row; cap to 3.
    let matches = grep(
        "data.csv",
        &csv,
        "note",
        &GrepOptions {
            ignore_case: false,
            limit: Some(3),
        },
    )
    .unwrap();
    assert_eq!(matches.len(), 3);
}
