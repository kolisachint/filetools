//! Generic XML handler — the lossless in-place core.
//!
//! Parses the source while tracking byte offsets, so every element is recorded
//! as exact spans in the original. Reconstruction never re-serializes the
//! document; it splices edits into those spans and copies everything else
//! through verbatim. That is what makes the round-trip byte-identical.
//!
//! drawio, OOXML parts, and PDF object streams are all XML/structured text and
//! build on this same machinery.

use std::collections::{BTreeMap, HashMap};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::idmap::{sha256_hex, IdMap, NodeLoc, Span};
use crate::model::{Attr, DocNode, Fidelity};

/// Generic XML handler.
pub struct XmlHandler;

impl super::Handler for XmlHandler {
    fn type_name(&self) -> &'static str {
        "xml"
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }
    fn extract(
        &self,
        bytes: &[u8],
        for_hash: &str,
    ) -> anyhow::Result<(Vec<DocNode>, Option<IdMap>)> {
        let e = extract(bytes, for_hash, "")?;
        Ok((e.nodes, Some(e.idmap)))
    }
}

/// Result of extracting an XML document.
pub struct Extracted {
    pub nodes: Vec<DocNode>,
    pub idmap: IdMap,
}

/// A frame on the open-element stack while parsing.
struct Frame {
    tag: String,
    element_start: usize,
    open_tag_end: usize,
    attrs_model: Vec<Attr>,
    attr_spans: BTreeMap<String, Span>,
    /// Set once we see any child element / comment / CDATA — disables the
    /// unambiguous single-text-run editing path.
    complex: bool,
    children: Vec<DocNode>,
    /// Per-tag child counter, for stable structural-path ids.
    sibling_counts: HashMap<String, u32>,
    path: String,
    id: String,
}

/// Extract one XML stream. `part` labels which container entry the bytes came
/// from (empty for a standalone file); it seeds structural-path ids so ids are
/// unique across parts, and is stamped onto every `NodeLoc`.
pub fn extract(bytes: &[u8], for_hash: &str, part: &str) -> anyhow::Result<Extracted> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut buf = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut roots: Vec<DocNode> = Vec::new();
    let mut root_counts: HashMap<String, u32> = HashMap::new();
    let mut idmap: BTreeMap<String, NodeLoc> = BTreeMap::new();
    let mut used_ids: HashMap<String, u32> = HashMap::new();

    loop {
        let start = reader.buffer_position() as usize;
        let ev = reader.read_event_into(&mut buf)?;
        let end = reader.buffer_position() as usize;

        match ev {
            Event::Start(ref e) => {
                let frame = open_frame(
                    e.name().as_ref(),
                    &bytes[start..end],
                    start,
                    end,
                    parent_path_and_counts(&mut stack, &mut root_counts, part),
                    &mut used_ids,
                );
                stack.push(frame);
            }
            Event::Empty(ref e) => {
                let frame = open_frame(
                    e.name().as_ref(),
                    &bytes[start..end],
                    start,
                    end,
                    parent_path_and_counts(&mut stack, &mut root_counts, part),
                    &mut used_ids,
                );
                // Self-closing: finalize immediately, no inner content.
                finalize(
                    frame, end, None, false, bytes, &mut idmap, &mut stack, &mut roots,
                );
            }
            Event::Text(_) => {
                if let Some(f) = stack.last_mut() {
                    // Track that we saw text; the inner span is derived later
                    // from the open/close tag boundaries, so nothing to store.
                    let _ = f;
                }
            }
            Event::CData(_) | Event::Comment(_) | Event::PI(_) => {
                if let Some(f) = stack.last_mut() {
                    f.complex = true;
                }
            }
            Event::End(_) => {
                if let Some(frame) = stack.pop() {
                    let close_tag_start = start;
                    finalize_close(
                        frame,
                        close_tag_start,
                        end,
                        bytes,
                        &mut idmap,
                        &mut stack,
                        &mut roots,
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    // Stamp the part label onto every recorded node.
    for loc in idmap.values_mut() {
        loc.part = part.to_string();
    }

    Ok(Extracted {
        nodes: roots,
        idmap: IdMap {
            for_hash: for_hash.to_string(),
            map: idmap,
        },
    })
}

/// Borrow the parent's path/counters (or the root counters) to assign a
/// stable structural-path id to a new child. Root-level elements seed their
/// path with `root_path` (the part name) so ids are unique across parts.
fn parent_path_and_counts<'a>(
    stack: &'a mut Vec<Frame>,
    root_counts: &'a mut HashMap<String, u32>,
    root_path: &str,
) -> (String, &'a mut HashMap<String, u32>) {
    match stack.last_mut() {
        Some(p) => {
            p.complex = true; // a parent with an element child is not a text leaf
            (p.path.clone(), &mut p.sibling_counts)
        }
        None => (root_path.to_string(), root_counts),
    }
}

fn open_frame(
    name: &[u8],
    tag_bytes: &[u8],
    element_start: usize,
    open_tag_end: usize,
    (parent_path, counts): (String, &mut HashMap<String, u32>),
    used_ids: &mut HashMap<String, u32>,
) -> Frame {
    let tag = String::from_utf8_lossy(name).into_owned();
    let idx = {
        let c = counts.entry(tag.clone()).or_insert(0);
        *c += 1;
        *c
    };
    let path = format!("{}/{}[{}]", parent_path, tag, idx);
    let id = stable_id(&path, used_ids);

    let (attrs_model, attr_spans) = parse_attrs(tag_bytes, element_start);

    Frame {
        tag,
        element_start,
        open_tag_end,
        attrs_model,
        attr_spans,
        complex: false,
        children: Vec::new(),
        sibling_counts: HashMap::new(),
        path,
        id,
    }
}

/// Finalize a self-closing element (Empty event).
#[allow(clippy::too_many_arguments)]
fn finalize(
    frame: Frame,
    element_end: usize,
    _inner: Option<Span>,
    _had_text: bool,
    bytes: &[u8],
    idmap: &mut BTreeMap<String, NodeLoc>,
    stack: &mut [Frame],
    roots: &mut Vec<DocNode>,
) {
    let element = Span {
        start: frame.element_start,
        end: element_end,
    };
    emit(frame, element, None, None, bytes, idmap, stack, roots);
}

/// Finalize a normal element on its closing tag.
fn finalize_close(
    frame: Frame,
    close_tag_start: usize,
    element_end: usize,
    bytes: &[u8],
    idmap: &mut BTreeMap<String, NodeLoc>,
    stack: &mut [Frame],
    roots: &mut Vec<DocNode>,
) {
    let element = Span {
        start: frame.element_start,
        end: element_end,
    };
    let inner = Span {
        start: frame.open_tag_end,
        end: close_tag_start,
    };
    // Only expose editable text for a single contiguous text run.
    let (inner_opt, text) = if frame.complex {
        (None, None)
    } else {
        let raw = &bytes[inner.start..inner.end];
        (Some(inner), Some(unescape(raw)))
    };
    emit(frame, element, inner_opt, text, bytes, idmap, stack, roots);
}

#[allow(clippy::too_many_arguments)]
fn emit(
    frame: Frame,
    element: Span,
    inner: Option<Span>,
    text: Option<String>,
    bytes: &[u8],
    idmap: &mut BTreeMap<String, NodeLoc>,
    stack: &mut [Frame],
    roots: &mut Vec<DocNode>,
) {
    let hash = sha256_hex(element.slice(bytes));
    idmap.insert(
        frame.id.clone(),
        NodeLoc {
            tag: frame.tag.clone(),
            part: String::new(), // stamped after the parse loop
            element,
            inner,
            runs: None,
            attrs: frame.attr_spans,
            hash,
        },
    );
    let node = DocNode {
        id: frame.id,
        tag: frame.tag,
        attrs: frame.attrs_model,
        text,
        children: frame.children,
    };
    match stack.last_mut() {
        Some(parent) => parent.children.push(node),
        None => roots.push(node),
    }
}

fn stable_id(path: &str, used: &mut HashMap<String, u32>) -> String {
    let h = sha256_hex(path.as_bytes());
    // `sha256:` is 7 chars; take 8 hex after it.
    let base = format!("el_{}", &h[7..15]);
    let n = used.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base
    } else {
        format!("{}_{}", base, n)
    }
}

/// Scan a raw start/empty tag for attribute name/value pairs, recording each
/// value's absolute byte span (excluding quotes).
fn parse_attrs(tag: &[u8], base: usize) -> (Vec<Attr>, BTreeMap<String, Span>) {
    let mut model = Vec::new();
    let mut spans = BTreeMap::new();
    let mut i = 1; // skip '<'
                   // skip tag name
    while i < tag.len() && !is_space(tag[i]) && tag[i] != b'>' && tag[i] != b'/' {
        i += 1;
    }
    loop {
        while i < tag.len() && is_space(tag[i]) {
            i += 1;
        }
        if i >= tag.len() || tag[i] == b'>' || tag[i] == b'/' {
            break;
        }
        let name_start = i;
        while i < tag.len() && tag[i] != b'=' && !is_space(tag[i]) && tag[i] != b'>' {
            i += 1;
        }
        let name = String::from_utf8_lossy(&tag[name_start..i]).into_owned();
        while i < tag.len() && is_space(tag[i]) {
            i += 1;
        }
        if i >= tag.len() || tag[i] != b'=' {
            continue; // valueless attr; ignore for v1
        }
        i += 1; // '='
        while i < tag.len() && is_space(tag[i]) {
            i += 1;
        }
        if i >= tag.len() {
            break;
        }
        let quote = tag[i];
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        i += 1;
        let val_start = i;
        while i < tag.len() && tag[i] != quote {
            i += 1;
        }
        let val_end = i;
        let value = unescape(&tag[val_start..val_end]);
        spans.insert(
            name.clone(),
            Span {
                start: base + val_start,
                end: base + val_end,
            },
        );
        model.push(Attr { name, value });
        if i < tag.len() {
            i += 1; // closing quote
        }
    }
    (model, spans)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Minimal entity unescape for *display* only. Reconstruction never relies on
/// this — it works on raw byte spans — so a lenient decode is fine here.
fn unescape(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
