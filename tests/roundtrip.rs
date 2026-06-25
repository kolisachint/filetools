//! End-to-end tests for extract -> patch -> reconstruct.

use filetools::model::Attr;
use filetools::patch::{NewElement, Op, Patch};
use filetools::{extract, reconstruct};

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
fn id_with_text(env: &filetools::model::Envelope, needle: &str) -> String {
    fn walk(nodes: &[filetools::model::DocNode], needle: &str) -> Option<String> {
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

fn id_with_attr(env: &filetools::model::Envelope, name: &str, value: &str) -> String {
    fn walk(nodes: &[filetools::model::DocNode], name: &str, value: &str) -> Option<String> {
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
    let result = reconstruct(&out.envelope, idmap, bytes, &Patch { patch: vec![] }).unwrap();
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
    let result = reconstruct(&out.envelope, idmap, bytes, &patch).unwrap();
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
    let s = String::from_utf8(reconstruct(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
    assert!(s.contains(r#"<meta author="Kolisachint"/>"#));
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
    let s = String::from_utf8(reconstruct(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
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
    let s = String::from_utf8(reconstruct(&out.envelope, idmap, bytes, &patch).unwrap()).unwrap();
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
    let err = reconstruct(&out.envelope, idmap, bytes, &patch).unwrap_err();
    assert!(err.to_string().contains("guard failed"), "got: {err}");
}

#[test]
fn drift_detected_on_reconstruct() {
    let bytes = SAMPLE.as_bytes();
    let out = extract("sample.xml", bytes).unwrap();
    let idmap = out.idmap.as_ref().unwrap();
    // Reconstruct against a *different* original than was extracted.
    let other = b"<doc/>";
    let err = reconstruct(&out.envelope, idmap, other, &Patch { patch: vec![] }).unwrap_err();
    assert!(err.to_string().contains("drifted"), "got: {err}");
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
    let s = String::from_utf8(reconstruct(&out.envelope, idmap, dio.as_bytes(), &patch).unwrap())
        .unwrap();
    assert!(s.contains(r#"value="Begin""#));
    assert!(s.contains(r#"id="2""#));
}
