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
    /// Full element span: from the `<` of the open tag through the `>` of the
    /// close tag (or the whole self-closing tag).
    pub element: Span,
    /// Inner text span between `>` and `</`. Absent for self-closing tags or
    /// elements with child content (text-replace only allowed when present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner: Option<Span>,
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
