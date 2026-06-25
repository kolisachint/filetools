//! drawio handler.
//!
//! Two shapes are supported:
//!
//!   * **Bare `<mxGraphModel>`** — the diagram XML sits directly in the file.
//!     Handled as plain whole-file XML by the lossless core (fully byte-lossless).
//!
//!   * **`<mxfile>` with `<diagram>` parts** — the real drawio format. Each
//!     `<diagram>` holds the model either inline (uncompressed XML) or, by
//!     default, **compressed**: `base64(deflateRaw(encodeURIComponent(xml)))`.
//!     This handler treats each diagram as a *part* whose editable stream is the
//!     decoded inner XML, mirroring the OOXML container approach:
//!       - extract: decode each diagram, run the XML core on the inner XML;
//!         id-map spans are inner-XML-relative, tagged with the diagram's part.
//!       - reconstruct: route ops to their diagram, splice the edits into the
//!         decoded inner XML (lossless there), re-encode (recompressing only
//!         edited diagrams), and splice the new blob into the outer file.
//!         Untouched diagrams keep their original blob byte-for-byte.
//!
//! So untouched diagrams are byte-identical; an *edited* diagram is recompressed
//! (deflate output isn't byte-stable), but its decoded XML differs from the
//! original only in the edited spans. A no-op patch returns the file unchanged.
//!
//! Diagram parts are keyed by position (`diagram:N`), recomputed deterministically
//! from the file on both extract and reconstruct, so nothing extra is persisted.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;

use super::xml;
use crate::idmap::{verify_spans, IdMap, Span};
use crate::model::{Attr, DocNode, Fidelity};
use crate::patch::{self, Op, Patch};

pub struct DrawioHandler;

impl super::Handler for DrawioHandler {
    fn type_name(&self) -> &'static str {
        "drawio"
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }

    fn extract(&self, bytes: &[u8], for_hash: &str) -> Result<(Vec<DocNode>, Option<IdMap>)> {
        let dgs = diagrams(bytes);
        if dgs.is_empty() {
            // Bare <mxGraphModel>: plain whole-file XML.
            let e = xml::extract(bytes, for_hash, "")?;
            return Ok((e.nodes, Some(e.idmap)));
        }

        // How many diagrams actually carry content (for the wrap decision).
        let non_empty = dgs.iter().filter(|d| !d.is_empty(bytes)).count();
        let multi = non_empty > 1;

        let mut structure: Vec<DocNode> = Vec::new();
        let mut merged: BTreeMap<String, _> = BTreeMap::new();

        for (idx, dg) in dgs.iter().enumerate() {
            let inner = decode_diagram(bytes, dg)?;
            if inner.is_empty() {
                continue;
            }
            let part = format!("diagram:{idx}");
            let e = xml::extract(&inner, for_hash, &part)?;
            for (id, loc) in e.idmap.map {
                merged.insert(id, loc);
            }
            if multi {
                structure.push(DocNode {
                    id: part.clone(),
                    tag: "_diagram".to_string(),
                    attrs: vec![Attr {
                        name: "index".to_string(),
                        value: idx.to_string(),
                    }],
                    text: None,
                    children: e.nodes,
                });
            } else {
                structure.extend(e.nodes);
            }
        }

        let idmap = IdMap {
            for_hash: for_hash.to_string(),
            map: merged,
        };
        Ok((structure, Some(idmap)))
    }

    fn verify(&self, bytes: &[u8], idmap: &IdMap) -> Result<()> {
        let dgs = diagrams(bytes);
        if dgs.is_empty() {
            return verify_spans(bytes, idmap);
        }
        for (part, sub) in group_by_part(idmap) {
            let dg = diagram_for_part(&dgs, &part)?;
            let inner = decode_diagram(bytes, dg)?;
            verify_spans(&inner, &sub)?;
        }
        Ok(())
    }

    fn reconstruct(&self, bytes: &[u8], idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>> {
        if patch.patch.is_empty() {
            return Ok(bytes.to_vec());
        }
        let dgs = diagrams(bytes);
        if dgs.is_empty() {
            return Ok(patch::apply(bytes, idmap, patch)?);
        }

        // Route each op to its diagram part.
        let mut by_part: BTreeMap<String, Vec<Op>> = BTreeMap::new();
        for op in &patch.patch {
            let id = op
                .target_id()
                .context("patch op has no resolvable target id")?;
            let loc = idmap
                .get(id)
                .ok_or_else(|| anyhow!("unknown node id `{id}`"))?;
            by_part
                .entry(loc.part.clone())
                .or_default()
                .push(op.clone());
        }

        // Apply per diagram in memory, then splice re-encoded blobs into the
        // outer file. Any failure aborts before the file is rebuilt.
        let mut edits: Vec<(Span, Vec<u8>)> = Vec::new();
        for (part, ops) in by_part {
            let dg = diagram_for_part(&dgs, &part)?;
            let inner = decode_diagram(bytes, dg)?;
            let new_inner = patch::apply(&inner, idmap, &Patch { patch: ops })?;
            if new_inner != inner {
                let blob = encode_diagram(&new_inner, dg.compressed);
                edits.push((dg.content, blob));
            }
        }

        if edits.is_empty() {
            return Ok(bytes.to_vec());
        }
        Ok(splice(bytes, edits))
    }
}

/// A `<diagram>` element's content span and whether it is compressed.
struct Diagram {
    content: Span,
    compressed: bool,
}

impl Diagram {
    fn is_empty(&self, outer: &[u8]) -> bool {
        outer[self.content.start..self.content.end]
            .iter()
            .all(|b| b.is_ascii_whitespace())
    }
}

/// Locate every `<diagram>…</diagram>` content span in the outer file.
fn diagrams(outer: &[u8]) -> Vec<Diagram> {
    let mut res = Vec::new();
    let mut i = 0;
    while let Some(p) = find(outer, b"<diagram", i) {
        let after = p + b"<diagram".len();
        // Require a tag boundary so we don't match a hypothetical `<diagrams`.
        match outer.get(after) {
            Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/') => {}
            _ => {
                i = after;
                continue;
            }
        }
        let Some(open_end) = find_byte(outer, b'>', after) else {
            break;
        };
        if outer.get(open_end.wrapping_sub(1)) == Some(&b'/') {
            // Self-closing <diagram/> — no content.
            i = open_end + 1;
            continue;
        }
        let content_start = open_end + 1;
        let Some(close) = find(outer, b"</diagram>", content_start) else {
            break;
        };
        let content = Span {
            start: content_start,
            end: close,
        };
        let compressed = !looks_xml(&outer[content.start..content.end]);
        res.push(Diagram {
            content,
            compressed,
        });
        i = close + b"</diagram>".len();
    }
    res
}

fn diagram_for_part<'a>(dgs: &'a [Diagram], part: &str) -> Result<&'a Diagram> {
    let idx: usize = part
        .strip_prefix("diagram:")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("bad diagram part `{part}`"))?;
    dgs.get(idx)
        .ok_or_else(|| anyhow!("diagram index {idx} out of range"))
}

/// Decode a diagram's inner XML. Inline diagrams return their bytes as-is;
/// compressed diagrams are base64 -> raw-inflate -> percent-decode.
fn decode_diagram(outer: &[u8], dg: &Diagram) -> Result<Vec<u8>> {
    let content = &outer[dg.content.start..dg.content.end];
    let trimmed = trim_ascii(content);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if !dg.compressed {
        return Ok(content.to_vec());
    }
    decompress_diagram(trimmed).context("decoding compressed <diagram>")
}

/// Re-encode inner XML for storage: inline returns it unchanged; compressed is
/// percent-encode -> raw-deflate -> base64.
fn encode_diagram(inner: &[u8], compressed: bool) -> Vec<u8> {
    if !compressed {
        inner.to_vec()
    } else {
        compress_diagram(inner).into_bytes()
    }
}

/// Decompress a drawio diagram blob: `base64(deflateRaw(encodeURIComponent(xml)))`.
pub fn decompress_diagram(blob: &[u8]) -> Result<Vec<u8>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .context("base64 decode")?;
    let mut inflated = Vec::new();
    DeflateDecoder::new(&raw[..])
        .read_to_end(&mut inflated)
        .context("raw inflate")?;
    Ok(percent_decode(&inflated))
}

/// Compress inner XML into a drawio diagram blob.
pub fn compress_diagram(xml: &[u8]) -> String {
    let encoded = percent_encode(xml);
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&encoded).expect("deflate write");
    let deflated = enc.finish().expect("deflate finish");
    base64::engine::general_purpose::STANDARD.encode(deflated)
}

// --- small helpers ----------------------------------------------------------

fn splice(original: &[u8], mut edits: Vec<(Span, Vec<u8>)>) -> Vec<u8> {
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    let mut out = Vec::with_capacity(original.len());
    let mut cursor = 0usize;
    for (sp, bytes) in &edits {
        out.extend_from_slice(&original[cursor..sp.start]);
        out.extend_from_slice(bytes);
        cursor = sp.end;
    }
    out.extend_from_slice(&original[cursor..]);
    out
}

fn group_by_part(idmap: &IdMap) -> BTreeMap<String, IdMap> {
    let mut out: BTreeMap<String, IdMap> = BTreeMap::new();
    for (id, loc) in &idmap.map {
        out.entry(loc.part.clone())
            .or_insert_with(|| IdMap {
                for_hash: idmap.for_hash.clone(),
                map: BTreeMap::new(),
            })
            .map
            .insert(id.clone(), loc.clone());
    }
    out
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn find_byte(hay: &[u8], b: u8, from: usize) -> Option<usize> {
    hay.iter()
        .skip(from)
        .position(|&x| x == b)
        .map(|p| p + from)
}

fn looks_xml(content: &[u8]) -> bool {
    trim_ascii(content).first() == Some(&b'<')
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = b.len();
    while start < end && b[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &b[start..end]
}

/// `decodeURIComponent`: replace `%XX` with the byte it encodes.
fn percent_decode(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `encodeURIComponent`: keep unreserved chars, percent-encode the rest.
fn percent_encode(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    for &c in b {
        let unreserved = c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if unreserved {
            out.push(c);
        } else {
            out.push(b'%');
            out.push(hex_digit(c >> 4));
            out.push(hex_digit(c & 0xf));
        }
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(v: u8) -> u8 {
    if v < 10 {
        b'0' + v
    } else {
        b'A' + (v - 10)
    }
}
