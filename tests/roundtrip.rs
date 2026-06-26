//! End-to-end tests for extract -> patch -> write.

use filetools_rs::model::Attr;
use filetools_rs::patch::{NewElement, Op, Patch};
use filetools_rs::{extract, write};

const SAMPLE: &str = r#"<?xml version="1.0"?>
<doc>
  <title>Quarterly Report</title>
  <section id="s1">
    <p>First paragraph.</p>
    <p>Second paragraph.</p>
  </section>
  <meta author="Sachin"/>
</doc>
"#;

/// Find the id of the first node whose text equals `needle`.
fn id_with_text(env: &filetools_rs::model::Envelope, needle: &str) -> String {
    fn walk(nodes: &[filetools_rs::model::DocNode], needle: &str) -> Option<String> {
        for n in nodes {
            if n.text.as_deref() == Some(needle) {
                return Some(n.id.clone());
            }
            if let Some(found) = walk(&n.children, needle) {
                return Some(found);
            }
        }
        None
    }
    walk(&env.structure, needle).expect("node with text not found")
}

/// Find the first node (at any depth) with the given tag.
fn node_with_tag<'a>(
    env: &'a filetools_rs::model::Envelope,
    tag: &str,
) -> &'a filetools_rs::model::DocNode {
    fn walk<'a>(
        nodes: &'a [filetools_rs::model::DocNode],
        tag: &str,
    ) -> Option<&'a filetools_rs::model::DocNode> {
        for n in nodes {
            if n.tag == tag {
                return Some(n);
            }
            if let Some(f) = walk(&n.children, tag) {
                return Some(f);
            }
        }
        None
    }
    walk(&env.structure, tag).expect("node with tag not found")
}

fn id_with_attr(env: &filetools_rs::model::Envelope, name: &str, value: &str) -> String {
    fn walk(nodes: &[filetools_rs::model::DocNode], name: &str, value: &str) -> Option<String> {
        for n in nodes {
            if n.attrs.iter().any(|a| a.name == name && a.value == value) {
                return Some(n.id.clone());
            }
            if let Some(f) = walk(&n.children, name, value) {
                return Some(f);
            }
        }
        None
    }
    walk(&env.structure, name, value).expect("node with attr not found")
}

#[test]
fn empty_patch_is_byte_identical() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let result = write(&out.envelope, idmap, bytes, &Patch { patch: vec![] }).unwrap();
    assert_eq!(
        result, bytes,
        "empty patch must reproduce the original exactly"
    );
}

#[test]
fn extract_is_lossless_and_writable() {
    let out = extract("sample.xml", SAMPLE.as_bytes()).unwrap();
    assert!(out.envelope.writable);
    assert_eq!(out.envelope.source.r#type, "xml");
    assert!(out.idmap.is_some());
}

#[test]
fn replace_text_is_surgical() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "First paragraph.");
    let guard = idmap.get(&id).unwrap().hash.clone();

    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: guard,
            },
            Op::Replace {
                path: format!("/structure/{id}/text"),
                value: "First paragraph, revised.".to_string(),
            },
        ],
    };
    let result = write(&out.envelope, idmap, bytes, &patch).unwrap();
    let s = String::from_utf8(result).unwrap();
    assert!(s.contains("<p>First paragraph, revised.</p>"));
    // Everything else untouched.
    assert!(s.contains("<p>Second paragraph.</p>"));
    assert!(s.contains(r#"<meta author="Sachin"/>"#));
    assert!(s.starts_with("<?xml version=\"1.0\"?>"));
}

#[test]
fn replace_attribute_value() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_attr(&out.envelope, "author", "Sachin");
    let guard = idmap.get(&id).unwrap().hash.clone();

    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: guard,
            },
            Op::Replace {
                path: format!("/structure/{id}/attrs/author"),
                value: "Kolisachint".to_string(),
            },
        ],
    };
    let s = String::from_utf8(write(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
    assert!(s.contains(r#"<meta author="Kolisachint"/>"#));
}

#[test]
fn replace_single_quoted_attr_with_apostrophe() {
    // Source attribute uses single quotes; the new value contains an
    // apostrophe, which must be escaped to keep the XML valid.
    let xml = r#"<doc><meta author='Sachin'/></doc>"#;
    let out = extract("q.xml", xml.as_bytes()).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_attr(&out.envelope, "author", "Sachin");
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/attrs/author"),
            value: "O'Brien".to_string(),
        }],
    };
    let s =
        String::from_utf8(write(&out.envelope, idmap, xml.as_bytes(), &patch).unwrap()).unwrap();
    assert!(s.contains("author='O&apos;Brien'"), "got: {s}");
    // The single-quote delimiters are intact and not broken by a raw apostrophe.
    assert!(!s.contains("'O'Brien'"));
}

#[test]
fn add_after_and_remove() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let first = id_with_text(&out.envelope, "First paragraph.");
    let second = id_with_text(&out.envelope, "Second paragraph.");

    let patch = Patch {
        patch: vec![
            Op::Add {
                after: Some(first.clone()),
                before: None,
                value: NewElement {
                    tag: "p".to_string(),
                    attrs: vec![],
                    text: Some("Inserted paragraph.".to_string()),
                },
            },
            Op::Remove {
                path: format!("/structure/{second}"),
            },
        ],
    };
    let s = String::from_utf8(write(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
    assert!(s.contains("<p>Inserted paragraph.</p>"));
    assert!(!s.contains("Second paragraph"));
    // Insert landed after the first paragraph.
    let fp = s.find("First paragraph.").unwrap();
    let ins = s.find("Inserted paragraph.").unwrap();
    assert!(fp < ins);
}

#[test]
fn add_with_attributes_serializes_correctly() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let first = id_with_text(&out.envelope, "First paragraph.");

    let patch = Patch {
        patch: vec![Op::Add {
            after: Some(first),
            before: None,
            value: NewElement {
                tag: "note".to_string(),
                attrs: vec![Attr {
                    name: "level".to_string(),
                    value: "info".to_string(),
                }],
                text: Some("see appendix".to_string()),
            },
        }],
    };
    let s = String::from_utf8(write(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
    assert!(s.contains(r#"<note level="info">see appendix</note>"#));
}

#[test]
fn stale_guard_aborts_atomically() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "First paragraph.");

    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: "sha256:deadbeef".to_string(),
            },
            Op::Replace {
                path: format!("/structure/{id}/text"),
                value: "should not apply".to_string(),
            },
        ],
    };
    let err = write(&out.envelope, idmap, bytes, &patch).unwrap_err();
    assert!(err.to_string().contains("guard failed"), "got: {err}");
}

#[test]
fn unknown_id_error_lists_valid_siblings() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();

    let patch = Patch {
        patch: vec![Op::Replace {
            path: "/structure/el_deadbeef/text".to_string(),
            value: "nope".to_string(),
        }],
    };
    let err = write(&out.envelope, idmap, bytes, &patch)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown node id `el_deadbeef`"), "got: {err}");
    assert!(err.contains("valid ids"), "got: {err}");
    // The recovered context must name real ids from the id-map.
    let some_valid = idmap.map.keys().next().unwrap();
    assert!(
        err.contains(some_valid) || idmap.map.keys().any(|k| err.contains(k)),
        "context should reference a real id-map id; got: {err}"
    );
}

// Preset B: drift no longer hard-fails. An out-of-band rewrite of the original
// triggers autonomous recovery — re-extract + fingerprint re-target — so a
// patch that can still be located is applied against the rewritten bytes.

#[test]
fn drift_empty_patch_is_noop_on_rewritten_original() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    // A *different* (rewritten) original; an empty patch must recover to a no-op.
    let other = b"<doc/>";
    let result = write(&out.envelope, idmap, other, &Patch { patch: vec![] }).unwrap();
    assert_eq!(
        result, other,
        "empty patch on drifted file should be a no-op"
    );
}

#[test]
fn drift_recovers_and_applies_when_content_survives() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "First paragraph.");

    // Simulate an out-of-band rewrite: same content, reformatted whitespace so
    // byte offsets (and thus the recorded id-map) no longer match.
    let rewritten = SAMPLE.replace("\n  ", "\n    ");
    assert_ne!(rewritten.as_bytes(), bytes, "rewrite must change the bytes");

    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "First paragraph, recovered.".to_string(),
        }],
    };
    let s = String::from_utf8(write(&out.envelope, idmap, rewritten.as_bytes(), &patch).unwrap())
        .unwrap();
    assert!(s.contains("First paragraph, recovered."), "got: {s}");
    assert!(
        s.contains("Second paragraph."),
        "other content preserved; got: {s}"
    );
}

#[test]
fn drift_aborts_when_target_content_gone() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "First paragraph.");

    // Rewrite that deletes the target paragraph entirely.
    let rewritten = SAMPLE.replace("    <p>First paragraph.</p>\n", "");
    assert!(!rewritten.contains("First paragraph."));

    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "should not land".to_string(),
        }],
    };
    let err = write(&out.envelope, idmap, rewritten.as_bytes(), &patch).unwrap_err();
    assert!(
        err.to_string().contains("no longer exists"),
        "expected fail-loud on vanished target; got: {err}"
    );
}

#[test]
fn unknown_binary_is_read_only() {
    let bytes = [0x00u8, 0x01, 0xff, b'h', b'e', b'l', b'l', b'o', 0x00];
    let out = extract("mystery.bin", &bytes).unwrap();
    assert!(!out.envelope.writable);
    assert!(out.idmap.is_none());
    assert_eq!(out.envelope.source.r#type, "binary");
}

#[test]
fn drawio_detected_and_roundtrips() {
    let dio =
        r#"<mxGraphModel><root><mxCell id="2" value="Start" vertex="1"/></root></mxGraphModel>"#;
    let out = extract("flow.drawio", dio.as_bytes()).unwrap();
    assert_eq!(out.envelope.source.r#type, "drawio");
    let idmap = out.idmap.as_ref().unwrap();
    // Relabel a cell via its value attribute.
    let id = id_with_attr(&out.envelope, "value", "Start");
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/attrs/value"),
            value: "Begin".to_string(),
        }],
    };
    let s =
        String::from_utf8(write(&out.envelope, idmap, dio.as_bytes(), &patch).unwrap()).unwrap();
    assert!(s.contains(r#"value="Begin""#));
    assert!(s.contains(r#"id="2""#));
}

// --- docx (OOXML container) -------------------------------------------------

use std::io::{Read, Write};

const DOCX_DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Original heading</w:t></w:r></w:p><w:p><w:r><w:t>Body text.</w:t></w:r></w:p></w:body></w:document>"#;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

/// Build a minimal but valid docx (three parts) in memory.
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

fn read_docx_part(container: &[u8], name: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(container)).unwrap();
    let mut f = zip.by_name(name).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

#[test]
fn docx_detected_and_lossless() {
    let docx = build_docx(DOCX_DOCUMENT);
    let out = extract("report.docx", &docx).unwrap();
    assert_eq!(out.envelope.source.r#type, "docx");
    assert!(out.envelope.writable);
    // Empty patch reproduces the container exactly.
    let same = write(
        &out.envelope,
        out.idmap.as_ref().unwrap(),
        &docx,
        &Patch { patch: vec![] },
    )
    .unwrap();
    assert_eq!(same, docx, "no-op docx patch must be byte-identical");
}

#[test]
fn docx_edit_is_surgical_and_repackages() {
    let docx = build_docx(DOCX_DOCUMENT);
    let out = extract("report.docx", &docx).unwrap();
    let idmap = out.idmap.as_ref().unwrap();

    // Edit the w:t run carrying the heading text.
    let id = id_with_text(&out.envelope, "Original heading");
    let guard = idmap.get(&id).unwrap().hash.clone();
    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: guard,
            },
            Op::Replace {
                path: format!("/structure/{id}/text"),
                value: "Revised heading".to_string(),
            },
        ],
    };
    let new_docx = write(&out.envelope, idmap, &docx, &patch).unwrap();

    // The edited part changed as intended...
    let doc = read_docx_part(&new_docx, "word/document.xml");
    assert!(doc.contains("<w:t>Revised heading</w:t>"));
    assert!(doc.contains("<w:t>Body text.</w:t>"));
    // ...and the untouched parts are preserved verbatim.
    assert_eq!(
        read_docx_part(&new_docx, "[Content_Types].xml"),
        CONTENT_TYPES
    );
    assert_eq!(read_docx_part(&new_docx, "_rels/.rels"), RELS);
    // Re-extracting the rebuilt docx still verifies (spans are consistent).
    extract("report.docx", &new_docx).unwrap();
}

// --- xlsx / pptx (multi-part containers) ------------------------------------

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

#[test]
fn xlsx_edits_shared_strings() {
    let shared = r#"<?xml version="1.0"?><sst xmlns="x" count="2"><si><t>Region</t></si><si><t>APAC</t></si></sst>"#;
    let workbook = r#"<?xml version="1.0"?><workbook/>"#;
    let xlsx = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", workbook),
        ("xl/sharedStrings.xml", shared),
    ]);

    let out = extract("book.xlsx", &xlsx).unwrap();
    assert_eq!(out.envelope.source.r#type, "xlsx");
    let idmap = out.idmap.as_ref().unwrap();

    let id = id_with_text(&out.envelope, "Region");
    // The node's part should be the shared-strings table.
    assert_eq!(idmap.get(&id).unwrap().part, "xl/sharedStrings.xml");
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "Area".to_string(),
        }],
    };
    let new = write(&out.envelope, idmap, &xlsx, &patch).unwrap();
    let s = read_docx_part(&new, "xl/sharedStrings.xml");
    assert!(s.contains("<t>Area</t>"));
    assert!(s.contains("<t>APAC</t>"));
    // Untouched part preserved.
    assert_eq!(read_docx_part(&new, "xl/workbook.xml"), workbook);
}

// Simulates the motivating openpyxl scenario: an external tool rewrites the
// whole workbook, re-emitting parts with different bytes (added attributes,
// reflowed markup) while preserving every cell value. Byte hashes all change;
// semantic recovery re-targets the patch by text signature and applies it.
#[test]
fn xlsx_drift_recovers_after_external_rewrite() {
    let shared = r#"<?xml version="1.0"?><sst xmlns="x" count="2"><si><t>Region</t></si><si><t>APAC</t></si></sst>"#;
    let workbook = r#"<?xml version="1.0"?><workbook/>"#;
    let xlsx = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", workbook),
        ("xl/sharedStrings.xml", shared),
    ]);

    let out = extract("book.xlsx", &xlsx).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "Region");

    // External rewrite: same values, re-emitted bytes (attrs + whitespace).
    let rewritten_shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="x" count="2" uniqueCount="2">
  <si><t xml:space="preserve">Region</t></si>
  <si><t xml:space="preserve">APAC</t></si>
</sst>"#;
    let rewritten = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", workbook),
        ("xl/sharedStrings.xml", rewritten_shared),
    ]);
    assert_ne!(rewritten, xlsx, "external rewrite must change the bytes");

    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "Area".to_string(),
        }],
    };
    // Patch authored against the original id-map, applied to drifted bytes.
    let new = write(&out.envelope, idmap, &rewritten, &patch).unwrap();
    let s = read_docx_part(&new, "xl/sharedStrings.xml");
    assert!(
        s.contains("Area"),
        "edit landed on recovered node; got: {s}"
    );
    assert!(s.contains("APAC"), "sibling value preserved; got: {s}");
}

#[test]
fn pptx_edits_multiple_slides_atomically() {
    let slide1 =
        r#"<?xml version="1.0"?><p:sld xmlns:p="p" xmlns:a="a"><a:t>Title One</a:t></p:sld>"#;
    let slide2 =
        r#"<?xml version="1.0"?><p:sld xmlns:p="p" xmlns:a="a"><a:t>Title Two</a:t></p:sld>"#;
    let pptx = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/slide2.xml", slide2),
        ("ppt/slides/_rels/slide1.xml.rels", RELS),
    ]);

    let out = extract("deck.pptx", &pptx).unwrap();
    assert_eq!(out.envelope.source.r#type, "pptx");
    let idmap = out.idmap.as_ref().unwrap();

    // Two slides => two synthetic _part wrappers in the structure.
    let part_markers = out
        .envelope
        .structure
        .iter()
        .filter(|n| n.tag == "_part")
        .count();
    assert_eq!(part_markers, 2);

    let id1 = id_with_text(&out.envelope, "Title One");
    let id2 = id_with_text(&out.envelope, "Title Two");
    assert_eq!(idmap.get(&id1).unwrap().part, "ppt/slides/slide1.xml");
    assert_eq!(idmap.get(&id2).unwrap().part, "ppt/slides/slide2.xml");

    // One patch spanning both slides — applied atomically across parts.
    let patch = Patch {
        patch: vec![
            Op::Replace {
                path: format!("/structure/{id1}/text"),
                value: "Opening".to_string(),
            },
            Op::Replace {
                path: format!("/structure/{id2}/text"),
                value: "Closing".to_string(),
            },
        ],
    };
    let new = write(&out.envelope, idmap, &pptx, &patch).unwrap();
    assert!(read_docx_part(&new, "ppt/slides/slide1.xml").contains("<a:t>Opening</a:t>"));
    assert!(read_docx_part(&new, "ppt/slides/slide2.xml").contains("<a:t>Closing</a:t>"));
    // The rels part (not selected) is preserved.
    assert_eq!(
        read_docx_part(&new, "ppt/slides/_rels/slide1.xml.rels"),
        RELS
    );
}

#[test]
fn docx_merges_runs_and_preserves_untouched_run() {
    // A paragraph split across two runs; the first run is bold.
    let doc = r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p></w:body></w:document>"#;
    let docx = build_docx(doc);
    let out = extract("merge.docx", &docx).unwrap();

    // The paragraph is presented as one merged string; its runs are hidden.
    let para = node_with_tag(&out.envelope, "w:p");
    assert_eq!(para.text.as_deref(), Some("Hello world"));
    assert!(
        para.children.is_empty(),
        "runs should be hidden under the paragraph"
    );

    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "Hello world");
    // The paragraph's id-map entry records its run spans.
    assert!(idmap.get(&id).unwrap().runs.is_some());

    // Insert a word in the middle of the merged text.
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "Hello brave world".to_string(),
        }],
    };
    let new = write(&out.envelope, idmap, &docx, &patch).unwrap();
    let d = read_docx_part(&new, "word/document.xml");

    // The untouched bold run is preserved byte-for-byte...
    assert!(
        d.contains("<w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r>"),
        "bold run must be untouched, got: {d}"
    );
    // ...and only the second run was rewritten to carry the change.
    assert!(d.contains("<w:t>brave world</w:t>"), "got: {d}");
    assert!(!d.contains("<w:t>world</w:t>"));
}

#[test]
fn pptx_cross_part_patch_aborts_atomically() {
    let slide1 =
        r#"<?xml version="1.0"?><p:sld xmlns:p="p" xmlns:a="a"><a:t>Title One</a:t></p:sld>"#;
    let slide2 =
        r#"<?xml version="1.0"?><p:sld xmlns:p="p" xmlns:a="a"><a:t>Title Two</a:t></p:sld>"#;
    let pptx = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("ppt/slides/slide1.xml", slide1),
        ("ppt/slides/slide2.xml", slide2),
    ]);
    let out = extract("deck.pptx", &pptx).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    let id1 = id_with_text(&out.envelope, "Title One");
    let id2 = id_with_text(&out.envelope, "Title Two");

    // A valid edit on slide1 plus a stale guard on slide2 must abort the whole
    // patch — the container is returned only if every part applied cleanly.
    let patch = Patch {
        patch: vec![
            Op::Replace {
                path: format!("/structure/{id1}/text"),
                value: "Opening".to_string(),
            },
            Op::Test {
                path: format!("/structure/{id2}"),
                hash: "sha256:bad".to_string(),
            },
        ],
    };
    assert!(write(&out.envelope, idmap, &pptx, &patch).is_err());
}

#[test]
fn xlsx_edits_worksheet_cell_value() {
    let shared = r#"<?xml version="1.0"?><sst xmlns="x"><si><t>Region</t></si></sst>"#;
    let workbook = r#"<?xml version="1.0"?><workbook/>"#;
    let sheet = r#"<?xml version="1.0"?><worksheet xmlns="x"><sheetData><row r="1"><c r="A1"><v>42</v></c></row></sheetData></worksheet>"#;
    let xlsx = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", workbook),
        ("xl/sharedStrings.xml", shared),
        ("xl/worksheets/sheet1.xml", sheet),
    ]);

    let out = extract("book.xlsx", &xlsx).unwrap();
    let idmap = out.idmap.as_ref().unwrap();

    // The numeric cell value is an editable node in the worksheet part.
    let id = id_with_text(&out.envelope, "42");
    assert_eq!(idmap.get(&id).unwrap().part, "xl/worksheets/sheet1.xml");
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/text"),
            value: "43".to_string(),
        }],
    };
    let new = write(&out.envelope, idmap, &xlsx, &patch).unwrap();
    assert!(read_docx_part(&new, "xl/worksheets/sheet1.xml").contains("<v>43</v>"));
    // sharedStrings untouched.
    assert_eq!(read_docx_part(&new, "xl/sharedStrings.xml"), shared);
}

// --- drawio compression -----------------------------------------------------

use filetools_rs::handlers::drawio::{compress_diagram, decompress_diagram};

const MODEL_A: &str =
    r#"<mxGraphModel><root><mxCell id="2" value="Start" vertex="1"/></root></mxGraphModel>"#;

#[test]
fn drawio_compression_roundtrips() {
    let blob = compress_diagram(MODEL_A.as_bytes());
    let back = decompress_diagram(blob.as_bytes()).unwrap();
    assert_eq!(back, MODEL_A.as_bytes());
}

#[test]
fn drawio_compressed_diagram_edits_and_reencodes() {
    let blob = compress_diagram(MODEL_A.as_bytes());
    let file =
        format!(r#"<mxfile host="app"><diagram id="d1" name="Page-1">{blob}</diagram></mxfile>"#);
    let bytes = file.as_bytes();

    let out = extract("flow.drawio", bytes).unwrap();
    assert_eq!(out.envelope.source.r#type, "drawio");
    let idmap = out.idmap.as_ref().unwrap();

    let id = id_with_attr(&out.envelope, "value", "Start");
    assert_eq!(idmap.get(&id).unwrap().part, "diagram:0");

    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/attrs/value"),
            value: "Begin".to_string(),
        }],
    };
    let new = write(&out.envelope, idmap, bytes, &patch).unwrap();
    let s = String::from_utf8(new.clone()).unwrap();
    // Outer mxfile structure preserved.
    assert!(s.starts_with(r#"<mxfile host="app">"#));
    assert!(s.contains(r#"<diagram id="d1" name="Page-1">"#));
    // Re-extracting decodes the recompressed blob and shows the edit.
    let out2 = extract("flow.drawio", &new).unwrap();
    let _ = id_with_attr(&out2.envelope, "value", "Begin"); // panics if missing
}

#[test]
fn drawio_decompresses_line_wrapped_base64() {
    // Some tools line-wrap the base64 blob; decoding must tolerate whitespace.
    let blob = compress_diagram(MODEL_A.as_bytes());
    let wrapped: String = blob
        .as_bytes()
        .chunks(40)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let back = decompress_diagram(wrapped.as_bytes()).unwrap();
    assert_eq!(back, MODEL_A.as_bytes());
}

#[test]
fn drawio_diagram_tag_with_gt_in_attribute() {
    // A `>` inside the diagram's name attribute must not be mistaken for the
    // end of the open tag.
    let blob = compress_diagram(MODEL_A.as_bytes());
    let file =
        format!(r#"<mxfile><diagram id="d1" name="A&gt;B is x>y">{blob}</diagram></mxfile>"#);
    let out = extract("gt.drawio", file.as_bytes()).unwrap();
    // Decoding the blob succeeded and the cell is visible.
    let _ = id_with_attr(&out.envelope, "value", "Start");
}

#[test]
fn drawio_untouched_diagram_blob_is_preserved() {
    let model_b =
        r#"<mxGraphModel><root><mxCell id="3" value="Bee" vertex="1"/></root></mxGraphModel>"#;
    let b1 = compress_diagram(MODEL_A.as_bytes());
    let b2 = compress_diagram(model_b.as_bytes());
    let file = format!(
        r#"<mxfile><diagram id="p1">{b1}</diagram><diagram id="p2">{b2}</diagram></mxfile>"#
    );
    let bytes = file.as_bytes();

    let out = extract("two.drawio", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    // Two diagrams => two _diagram markers.
    assert_eq!(
        out.envelope
            .structure
            .iter()
            .filter(|n| n.tag == "_diagram")
            .count(),
        2
    );

    // Edit only the first diagram's cell.
    let id = id_with_attr(&out.envelope, "value", "Start");
    assert_eq!(idmap.get(&id).unwrap().part, "diagram:0");
    let patch = Patch {
        patch: vec![Op::Replace {
            path: format!("/structure/{id}/attrs/value"),
            value: "Started".to_string(),
        }],
    };
    let new = write(&out.envelope, idmap, bytes, &patch).unwrap();
    let s = String::from_utf8(new).unwrap();
    // The second, untouched diagram keeps its exact original blob.
    assert!(
        s.contains(&b2),
        "untouched diagram blob must be byte-identical"
    );
    // The first diagram's blob changed (recompressed with the edit).
    assert!(!s.contains(&b1));
}

// --- PDF (InPlaceText) ------------------------------------------------------

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

/// Build a one-page PDF drawing each string with its own `Tj`.
fn build_pdf(texts: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
    });
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
    ];
    let mut y = 700;
    for t in texts {
        ops.push(Operation::new("Td", vec![72.into(), y.into()]));
        ops.push(Operation::new("Tj", vec![Object::string_literal(*t)]));
        y -= 40;
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

#[test]
fn pdf_text_edit_preserves_other_text() {
    let pdf = build_pdf(&["Hello World!", "Second line"]);
    let out = extract("doc.pdf", &pdf).unwrap();
    assert_eq!(out.envelope.source.r#type, "pdf");
    assert_eq!(
        out.envelope.fidelity,
        filetools_rs::model::Fidelity::InPlaceText
    );

    let idmap = out.idmap.as_ref().unwrap();
    let id = id_with_text(&out.envelope, "Hello World!");
    let guard = idmap.get(&id).unwrap().hash.clone();
    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: guard,
            },
            Op::Replace {
                path: format!("/structure/{id}/text"),
                value: "Goodbye World!".to_string(),
            },
        ],
    };
    let new = write(&out.envelope, idmap, &pdf, &patch).unwrap();

    // Re-extract the rebuilt PDF: the edit landed, the other string is intact.
    let out2 = extract("doc.pdf", &new).unwrap();
    let _ = id_with_text(&out2.envelope, "Goodbye World!"); // panics if missing
    let _ = id_with_text(&out2.envelope, "Second line"); // untouched, preserved
}

#[test]
fn pdf_stale_guard_aborts() {
    let pdf = build_pdf(&["Hello"]);
    let out = extract("d.pdf", &pdf).unwrap();
    let id = id_with_text(&out.envelope, "Hello");
    let patch = Patch {
        patch: vec![
            Op::Test {
                path: format!("/structure/{id}"),
                hash: "sha256:bad".to_string(),
            },
            Op::Replace {
                path: format!("/structure/{id}/text"),
                value: "X".to_string(),
            },
        ],
    };
    assert!(write(&out.envelope, out.idmap.as_ref().unwrap(), &pdf, &patch).is_err());
}

#[test]
fn pdf_rejects_structural_ops() {
    let pdf = build_pdf(&["Hello"]);
    let out = extract("d.pdf", &pdf).unwrap();
    let id = id_with_text(&out.envelope, "Hello");
    let patch = Patch {
        patch: vec![Op::Remove {
            path: format!("/structure/{id}"),
        }],
    };
    assert!(write(&out.envelope, out.idmap.as_ref().unwrap(), &pdf, &patch).is_err());
}

// --- grep (discovery) -------------------------------------------------------

#[test]
fn grep_finds_block_ids_for_matching_text() {
    use filetools_rs::grep;
    use filetools_rs::model::GrepOptions;

    let bytes = SAMPLE.as_bytes();
    let matches = grep("sample.xml", bytes, "paragraph", &GrepOptions::default()).unwrap();
    // "First paragraph." and "Second paragraph." both match.
    assert_eq!(matches.len(), 2, "got: {matches:?}");
    assert!(matches.iter().all(|m| m.writable), "xml is writable");

    // Returned ids resolve via read() back to the matching nodes.
    let out = filetools_rs::extract("sample.xml", bytes).unwrap();
    let first = id_with_text(&out.envelope, "First paragraph.");
    let second = id_with_text(&out.envelope, "Second paragraph.");
    let ids: Vec<&str> = matches.iter().map(|m| m.block_id.as_str()).collect();
    assert!(ids.contains(&first.as_str()));
    assert!(ids.contains(&second.as_str()));
}

#[test]
fn grep_ignore_case_and_limit() {
    use filetools_rs::grep;
    use filetools_rs::model::GrepOptions;

    let bytes = SAMPLE.as_bytes();

    // Case-sensitive: "QUARTERLY" matches nothing.
    let none = grep("s.xml", bytes, "QUARTERLY", &GrepOptions::default()).unwrap();
    assert!(none.is_empty());

    // Case-insensitive: matches the title.
    let ci = grep(
        "s.xml",
        bytes,
        "QUARTERLY",
        &GrepOptions {
            ignore_case: true,
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(ci.len(), 1);

    // Limit caps the result count.
    let capped = grep(
        "s.xml",
        bytes,
        "paragraph",
        &GrepOptions {
            ignore_case: false,
            limit: Some(1),
        },
    )
    .unwrap();
    assert_eq!(capped.len(), 1);
}
