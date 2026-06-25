//! The sidecar id-map: stable node id -> byte ranges in the original file.
//!
//! This is what makes in-place mutation possible. The original is never
//! modified on extract; instead we record exactly which byte spans each
//! node occupies, so reconstruct can splice edits in surgically and leave
//! everything else byte-for-byte intact.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A half-open byte range `[start, end)` into the original file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn slice<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.start..self.end]
    }
}

/// Where a single node lives in the original bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLoc {
    pub tag: String,
    /// For container formats (OOXML), the zip entry the spans index into
    /// (e.g. `word/document.xml`, `ppt/slides/slide1.xml`). Empty for
    /// single-stream formats where spans index into the whole file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part: String,
    /// Full element span: from the `<` of the open tag through the `>` of the
    /// close tag (or the whole self-closing tag).
    pub element: Span,
    /// Inner text span between `>` and `</`. Absent for self-closing tags or
    /// elements with child content (text-replace only allowed when present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner: Option<Span>,
    /// For merged paragraph nodes (OOXML): the ordered inner spans of the text
    /// runs (`w:t`/`a:t`/`t`) whose concatenation forms the paragraph text.
    /// A text-replace diffs old-vs-new and rewrites only the affected runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<Span>>,
    /// Byte range of each attribute *value* (excluding quotes), keyed by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, Span>,
    /// `sha256:<hex>` of the full element bytes, for hash-guarded ops.
    pub hash: String,
}

/// The sidecar document. Bound to a specific original via `for_hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdMap {
    /// `sha256:<hex>` of the original this map describes.
    pub for_hash: String,
    pub map: BTreeMap<String, NodeLoc>,
}

impl IdMap {
    pub fn get(&self, id: &str) -> Option<&NodeLoc> {
        self.map.get(id)
    }
}

/// Confirm an id-map is self-consistent against the byte stream its spans
/// index into: each element span is in bounds, starts at `<`, names the
/// recorded tag, and hashes to the stored guard. Catches off-by-one span bugs
/// before any edit relies on them. (Does not check `for_hash`, which binds to
/// the *container* and is verified separately.)
pub fn verify_spans(stream: &[u8], map: &IdMap) -> anyhow::Result<()> {
    use anyhow::bail;
    for (id, loc) in &map.map {
        let el = loc.element;
        if el.start >= el.end || el.end > stream.len() {
            bail!("node `{id}`: span out of bounds");
        }
        let slice = &stream[el.start..el.end];
        if slice.first() != Some(&b'<') {
            bail!("node `{id}`: span does not start at an element");
        }
        if !slice[1..].starts_with(loc.tag.as_bytes()) {
            bail!("node `{id}`: span tag mismatch (expected `{}`)", loc.tag);
        }
        if sha256_hex(slice) != loc.hash {
            bail!("node `{id}`: hash mismatch");
        }
        if let Some(inner) = loc.inner {
            if inner.start < el.start || inner.end > el.end || inner.start > inner.end {
                bail!("node `{id}`: inner span outside element");
            }
        }
        if let Some(runs) = &loc.runs {
            for r in runs {
                if r.start < el.start || r.end > el.end || r.start > r.end {
                    bail!("node `{id}`: run span outside element");
                }
            }
        }
        for (name, span) in &loc.attrs {
            if span.start < el.start || span.end > el.end || span.start > span.end {
                bail!("node `{id}`: attr `{name}` span outside element");
            }
        }
    }
    Ok(())
}

/// `sha256:<hex>` of arbitrary bytes, in the canonical prefixed form.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(7 + digest.len() * 2);
    s.push_str("sha256:");
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
