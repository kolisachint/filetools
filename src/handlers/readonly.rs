//! Read-only fallback for unknown/unsupported binary formats.
//!
//! Per the design: best-effort text extraction, no write-back. Output is
//! flagged `writable: false` and carries no id-map, so reconstruct refuses it
//! rather than risk a lossy/corrupt round-trip.

use crate::idmap::IdMap;
use crate::model::{DocNode, Fidelity};

pub struct ReadOnlyHandler;

impl super::Handler for ReadOnlyHandler {
    fn type_name(&self) -> &'static str {
        "binary"
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::ReadOnly
    }
    fn extract(
        &self,
        bytes: &[u8],
        _for_hash: &str,
    ) -> anyhow::Result<(Vec<DocNode>, Option<IdMap>)> {
        let text = extract_text(bytes);
        let node = DocNode {
            id: "text".to_string(),
            tag: "text".to_string(),
            attrs: Vec::new(),
            text: Some(text),
            children: Vec::new(),
        };
        Ok((vec![node], None))
    }
}

/// If the bytes are valid UTF-8, return them; otherwise pull printable ASCII
/// runs of length >= 4 (the classic `strings(1)` heuristic).
fn extract_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut out = String::new();
    let mut run = String::new();
    for &b in bytes {
        if (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\t' {
            run.push(b as char);
        } else {
            if run.trim().len() >= 4 {
                out.push_str(run.trim());
                out.push('\n');
            }
            run.clear();
        }
    }
    if run.trim().len() >= 4 {
        out.push_str(run.trim());
    }
    out
}
