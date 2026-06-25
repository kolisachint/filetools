//! drawio handler — a thin semantic layer over the generic XML core.
//!
//! `.drawio` files are mxGraph XML where every `<mxCell>` already carries a
//! stable `id` attribute and human-meaningful `value` (label) and geometry.
//! Extraction is identical to generic XML; we only relabel the format so the
//! envelope and downstream tooling can distinguish diagrams. All editing
//! (relabel a cell via `attrs/value`, add/remove cells) flows through the same
//! lossless byte-splice path.
//!
//! Note: drawio files can be stored compressed (base64+deflate inside the
//! `<diagram>` element). This handler targets the uncompressed form; detecting
//! and inflating the compressed variant is a follow-up.

use crate::idmap::IdMap;
use crate::model::{DocNode, Fidelity};

pub struct DrawioHandler;

impl super::Handler for DrawioHandler {
    fn type_name(&self) -> &'static str {
        "drawio"
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }
    fn extract(
        &self,
        bytes: &[u8],
        for_hash: &str,
    ) -> anyhow::Result<(Vec<DocNode>, Option<IdMap>)> {
        let e = super::xml::extract(bytes, for_hash, "")?;
        Ok((e.nodes, Some(e.idmap)))
    }
}

// Re-exported helpers could surface cell-level semantics (nodes vs edges) in a
// later pass; for v1 the generic element view already exposes `id`/`value`.
#[allow(dead_code)]
fn _doc_marker(_: &[DocNode], _: &IdMap) {}
