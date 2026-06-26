//! HTML handler: span-tracking tokenizer for headings, title, and paragraphs.
//!
//! Rather than a full DOM parser, this scans for a small set of text-bearing
//! elements and records exact byte spans for each, so edits are surgical and
//! every untouched byte is preserved. Because HTML elements start with `<`, the
//! generic `verify_spans` and the XML patch applier work unchanged.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::idmap::{sha256_hex, IdMap, NodeLoc, Span};
use crate::model::{Attr, DocNode, Fidelity};

/// One text-bearing element located in the source.
pub struct HtmlElement {
    pub id: String,
    pub tag: String,
    /// `[start, end)` of the whole element, `<tag ...>...</tag>`.
    pub element: Span,
    /// `[start, end)` of the inner text between `>` and `</`.
    pub inner: Span,
    /// Decoded inner text (entities left as-is; preview only).
    pub text: String,
}

/// Tokenize the editable elements of an HTML document in document order.
///
/// Recognizes `<title>`, `<h1>`..`<h6>`, and `<p>` with a matching close tag
/// and simple (non-nested) text content. Elements containing nested tags are
/// skipped for editing (still safe: they just are not addressable), keeping
/// text-replace unambiguous.
pub fn tokenize(content: &str) -> Vec<HtmlElement> {
    let bytes = content.as_bytes();
    let lower = content.to_ascii_lowercase();
    let lb = lower.as_bytes();

    let mut elements = Vec::new();
    let mut heading_idx = 0usize;
    let mut para_idx = 0usize;
    let mut title_done = false;
    let mut i = 0usize;

    while i < lb.len() {
        if lb[i] != b'<' {
            i += 1;
            continue;
        }
        // Identify the tag name after '<'.
        let name_start = i + 1;
        if name_start >= lb.len() || !lb[name_start].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let mut j = name_start;
        while j < lb.len() && (lb[j].is_ascii_alphanumeric()) {
            j += 1;
        }
        let name = &lower[name_start..j];

        let kind = classify(name, title_done);
        let Some(kind) = kind else {
            i += 1;
            continue;
        };

        // Find end of the opening tag '>'.
        let Some(gt_rel) = lower[j..].find('>') else {
            break;
        };
        let open_end = j + gt_rel + 1;
        // Void/self-closing opening tag: no inner text to edit.
        if lb.get(open_end.wrapping_sub(2)) == Some(&b'/') {
            i = open_end;
            continue;
        }

        let close = format!("</{name}>");
        let Some(close_rel) = lower[open_end..].find(&close) else {
            i = open_end;
            continue;
        };
        let inner_start = open_end;
        let inner_end = open_end + close_rel;
        let element_end = inner_end + close.len();

        // Skip elements whose inner content contains nested tags: text-replace
        // would be ambiguous, so we leave them unaddressable rather than risk a
        // lossy edit.
        if lower[inner_start..inner_end].contains('<') {
            i = element_end;
            continue;
        }

        let id = match kind {
            Kind::Title => {
                title_done = true;
                "title".to_string()
            }
            Kind::Heading => {
                let id = format!("section[{heading_idx}]");
                heading_idx += 1;
                id
            }
            Kind::Paragraph => {
                let id = format!("paragraph[{para_idx}]");
                para_idx += 1;
                id
            }
        };

        elements.push(HtmlElement {
            id,
            tag: name.to_string(),
            element: Span {
                start: i,
                end: element_end,
            },
            inner: Span {
                start: inner_start,
                end: inner_end,
            },
            text: content[inner_start..inner_end].to_string(),
        });

        i = element_end;
    }

    // Ensure the spans we recorded actually index into `bytes` (ASCII '<'/'>'
    // are single-byte, so byte offsets from the lowercased copy are valid on
    // the original too).
    debug_assert!(elements.iter().all(|e| e.element.end <= bytes.len()));
    elements
}

enum Kind {
    Title,
    Heading,
    Paragraph,
}

fn classify(name: &str, title_done: bool) -> Option<Kind> {
    match name {
        "title" if !title_done => Some(Kind::Title),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Kind::Heading),
        "p" => Some(Kind::Paragraph),
        _ => None,
    }
}

/// The HTML handler: writable, byte-faithful text edits on headings/title/paras.
pub struct HtmlHandler;

impl super::Handler for HtmlHandler {
    fn type_name(&self) -> &'static str {
        "html"
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }

    fn extract(&self, bytes: &[u8], for_hash: &str) -> Result<(Vec<DocNode>, Option<IdMap>)> {
        let content = std::str::from_utf8(bytes)
            .map_err(|e| anyhow::anyhow!("invalid UTF-8 in HTML: {e}"))?;
        let elements = tokenize(content);

        let mut structure = Vec::new();
        let mut map = BTreeMap::new();

        for el in &elements {
            structure.push(DocNode {
                id: el.id.clone(),
                tag: el.tag.clone(),
                attrs: vec![Attr {
                    name: "level".to_string(),
                    value: el.tag.clone(),
                }],
                text: Some(el.text.clone()),
                children: Vec::new(),
            });

            let element_bytes = &bytes[el.element.start..el.element.end];
            map.insert(
                el.id.clone(),
                NodeLoc {
                    tag: el.tag.clone(),
                    part: String::new(),
                    element: el.element,
                    inner: Some(el.inner),
                    runs: None,
                    attrs: BTreeMap::new(),
                    hash: sha256_hex(element_bytes),
                },
            );
        }

        let idmap = IdMap {
            for_hash: for_hash.to_string(),
            map,
        };
        Ok((structure, Some(idmap)))
    }
}
