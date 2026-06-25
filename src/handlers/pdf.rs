//! PDF handler — `InPlaceText` fidelity.
//!
//! PDF has no XML core to lean on: text is drawn by operators (`Tj`, `TJ`, `'`,
//! `"`) inside compressed content streams. This handler uses `lopdf` to parse
//! the document, decode each page's content stream, and expose the literal
//! strings as editable text nodes.
//!
//! Reconstruct replaces those strings in place and re-encodes the page's
//! content stream, leaving every glyph position untouched — so this is a
//! **layout-preserving, text-level** edit, never a reflow. Text ids are derived
//! deterministically from page/string position, so they are recomputed on both
//! extract and reconstruct; nothing positional is persisted. Hash guards work
//! against the current string bytes.
//!
//! Scope/limits (v1): only text replacement is supported (no add/remove/attr).
//! Strings are treated as Latin-1/ASCII bytes; documents using custom font
//! encodings or `ToUnicode` maps may not display or re-encode non-ASCII text
//! faithfully. Length-changing edits are allowed but, since there is no reflow,
//! a much longer string may overrun its visual box.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::idmap::{sha256_hex, IdMap, NodeLoc, Span};
use crate::model::{Attr, DocNode, Fidelity};
use crate::patch::{Op, Patch};

pub struct PdfHandler;

impl super::Handler for PdfHandler {
    fn type_name(&self) -> &'static str {
        "pdf"
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::InPlaceText
    }

    fn extract(&self, bytes: &[u8], for_hash: &str) -> Result<(Vec<DocNode>, Option<IdMap>)> {
        let doc = Document::load_mem(bytes).context("parsing PDF")?;
        let pages = ordered_pages(&doc);
        let multi = pages.len() > 1;

        let mut structure: Vec<DocNode> = Vec::new();
        let mut map: BTreeMap<String, NodeLoc> = BTreeMap::new();

        for (pi, pid) in pages.iter().enumerate() {
            let mut content = doc
                .get_and_decode_page_content(*pid)
                .with_context(|| format!("decoding page {pi} content"))?;
            let part = format!("page:{pi}");
            let mut nodes = Vec::new();

            for_each_string(&mut content, |ti, s| {
                let id = format!("pdf_p{pi}_t{ti}");
                map.insert(
                    id.clone(),
                    NodeLoc {
                        tag: "text".to_string(),
                        part: part.clone(),
                        element: Span { start: 0, end: 0 }, // PDF spans aren't byte ranges
                        inner: None,
                        runs: None,
                        attrs: Default::default(),
                        hash: sha256_hex(s),
                    },
                );
                nodes.push(DocNode {
                    id,
                    tag: "text".to_string(),
                    attrs: Vec::new(),
                    text: Some(String::from_utf8_lossy(s).into_owned()),
                    children: Vec::new(),
                });
            });

            if multi {
                structure.push(DocNode {
                    id: part.clone(),
                    tag: "_page".to_string(),
                    attrs: vec![Attr {
                        name: "index".to_string(),
                        value: pi.to_string(),
                    }],
                    text: None,
                    children: nodes,
                });
            } else {
                structure.extend(nodes);
            }
        }

        Ok((
            structure,
            Some(IdMap {
                for_hash: for_hash.to_string(),
                map,
            }),
        ))
    }

    fn verify(&self, _bytes: &[u8], _idmap: &IdMap) -> Result<()> {
        // InPlaceText: id-map entries carry guard hashes, not byte spans, so
        // there is nothing to byte-validate here.
        Ok(())
    }

    fn reconstruct(&self, bytes: &[u8], _idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>> {
        // Collect text replacements and guards; reject anything else.
        let mut repl: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut guards: Vec<(String, String)> = Vec::new();
        for op in &patch.patch {
            match op {
                Op::Test { path, hash } => {
                    guards.push((pointer_id(path)?.to_string(), hash.clone()));
                }
                Op::Replace { path, value } => {
                    let id = text_pointer_id(path)?;
                    repl.insert(id.to_string(), value.clone().into_bytes());
                }
                _ => bail!("PDF supports only text `replace` (add/remove/attr not allowed)"),
            }
        }
        if repl.is_empty() && guards.is_empty() {
            return Ok(bytes.to_vec());
        }

        let mut doc = Document::load_mem(bytes).context("parsing PDF")?;
        let pages = ordered_pages(&doc);

        // Guard + existence checks against current text (read pass) before any
        // mutation, so a stale guard aborts atomically.
        let current = current_text(&doc, &pages)?;
        for (id, hash) in &guards {
            let cur = current
                .get(id)
                .ok_or_else(|| anyhow!("unknown node id `{id}`"))?;
            if &sha256_hex(cur) != hash {
                bail!("guard failed for `{id}`");
            }
        }
        for id in repl.keys() {
            if !current.contains_key(id) {
                bail!("unknown node id `{id}`");
            }
        }
        if repl.is_empty() {
            return Ok(bytes.to_vec());
        }

        // Mutate each page's content and re-point /Contents at a fresh stream.
        for (pi, pid) in pages.iter().enumerate() {
            let mut content = doc.get_and_decode_page_content(*pid)?;
            let mut changed = false;
            for_each_string(&mut content, |ti, s| {
                if let Some(nv) = repl.get(&format!("pdf_p{pi}_t{ti}")) {
                    *s = nv.clone();
                    changed = true;
                }
            });
            if changed {
                let encoded = content.encode().context("re-encoding page content")?;
                let mut stream = Stream::new(Dictionary::new(), encoded);
                let _ = stream.compress();
                let new_id = doc.add_object(Object::Stream(stream));
                if let Ok(Object::Dictionary(d)) = doc.get_object_mut(*pid) {
                    d.set("Contents", Object::Reference(new_id));
                }
            }
        }

        let mut out = Vec::new();
        doc.save_to(&mut out).context("writing PDF")?;
        Ok(out)
    }
}

/// Page object ids in page order.
fn ordered_pages(doc: &Document) -> Vec<ObjectId> {
    doc.get_pages().into_values().collect()
}

/// Visit every literal string drawn by a text operator, in order, handing the
/// callback a running index and a mutable reference to the string bytes.
fn for_each_string<F: FnMut(usize, &mut Vec<u8>)>(content: &mut Content, mut f: F) {
    let mut counter = 0usize;
    for op in &mut content.operations {
        match op.operator.as_str() {
            // Tj / ' / " each draw a single string operand.
            "Tj" | "'" | "\"" => {
                for obj in &mut op.operands {
                    if let Object::String(s, _) = obj {
                        f(counter, s);
                        counter += 1;
                        break;
                    }
                }
            }
            // TJ draws an array interleaving strings and spacing numbers.
            "TJ" => {
                for obj in &mut op.operands {
                    if let Object::Array(arr) = obj {
                        for el in arr {
                            if let Object::String(s, _) = el {
                                f(counter, s);
                                counter += 1;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Build the current id -> string-bytes map (read-only pass).
fn current_text(doc: &Document, pages: &[ObjectId]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    for (pi, pid) in pages.iter().enumerate() {
        let mut content = doc.get_and_decode_page_content(*pid)?;
        for_each_string(&mut content, |ti, s| {
            out.insert(format!("pdf_p{pi}_t{ti}"), s.clone());
        });
    }
    Ok(out)
}

/// `/structure/<id>` or `/structure/<id>/...` -> `<id>`.
fn pointer_id(path: &str) -> Result<&str> {
    path.strip_prefix("/structure/")
        .and_then(|r| r.split('/').next())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("bad pointer `{path}`"))
}

/// `/structure/<id>/text` -> `<id>`; rejects anything but a `/text` target.
fn text_pointer_id(path: &str) -> Result<&str> {
    let rest = path
        .strip_prefix("/structure/")
        .ok_or_else(|| anyhow!("bad pointer `{path}`"))?;
    let mut parts = rest.splitn(2, '/');
    let id = parts.next().filter(|s| !s.is_empty());
    match (id, parts.next()) {
        (Some(id), Some("text")) => Ok(id),
        _ => bail!("PDF replace must target `/structure/<id>/text`, got `{path}`"),
    }
}
