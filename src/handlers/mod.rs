//! Format handlers and dispatch.

pub mod drawio;
pub mod ooxml;
pub mod readonly;
pub mod xml;

use crate::idmap::{verify_spans, IdMap};
use crate::model::{DocNode, Fidelity};
use crate::patch::{self, Patch};

/// A format handler. Every handler resolves nodes back to byte spans (via the
/// id-map) except read-only ones, which extract text for context only.
///
/// The default `verify`/`reconstruct` treat the whole file as the byte stream
/// the spans index into (correct for plain XML/drawio). Container formats like
/// docx override them to operate on an inner part and repackage.
pub trait Handler {
    /// Logical format name recorded in the envelope (`xml`, `drawio`, ...).
    fn type_name(&self) -> &'static str;
    fn fidelity(&self) -> Fidelity;
    /// Extract semantic nodes and, for writable formats, the sidecar id-map.
    fn extract(
        &self,
        bytes: &[u8],
        for_hash: &str,
    ) -> anyhow::Result<(Vec<DocNode>, Option<IdMap>)>;

    /// Verify the id-map's spans against the stream they index into.
    fn verify(&self, bytes: &[u8], idmap: &IdMap) -> anyhow::Result<()> {
        verify_spans(bytes, idmap)
    }

    /// Apply a patch and return the reconstructed file bytes.
    fn reconstruct(&self, bytes: &[u8], idmap: &IdMap, patch: &Patch) -> anyhow::Result<Vec<u8>> {
        Ok(patch::apply(bytes, idmap, patch)?)
    }
}

/// Pick a handler from the file extension, falling back to a content sniff and
/// finally to best-effort read-only text extraction.
pub fn for_path(path: &str, bytes: &[u8]) -> Box<dyn Handler> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "docx" => Box::new(ooxml::docx()),
        "xlsx" => Box::new(ooxml::xlsx()),
        "pptx" => Box::new(ooxml::pptx()),
        "drawio" | "dio" => Box::new(drawio::DrawioHandler),
        "xml" | "svg" | "xhtml" => Box::new(xml::XmlHandler),
        _ => {
            if looks_like_xml(bytes) {
                Box::new(xml::XmlHandler)
            } else {
                Box::new(readonly::ReadOnlyHandler)
            }
        }
    }
}

/// Reconstruct a handler from the `type` recorded in an envelope, so
/// reconstruct uses the same machinery extract did.
pub fn for_type(type_name: &str) -> Option<Box<dyn Handler>> {
    match type_name {
        "docx" => Some(Box::new(ooxml::docx())),
        "xlsx" => Some(Box::new(ooxml::xlsx())),
        "pptx" => Some(Box::new(ooxml::pptx())),
        "drawio" => Some(Box::new(drawio::DrawioHandler)),
        "xml" => Some(Box::new(xml::XmlHandler)),
        "binary" => Some(Box::new(readonly::ReadOnlyHandler)),
        _ => None,
    }
}

fn looks_like_xml(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(256)];
    let mut i = 0;
    while i < head.len() && head[i].is_ascii_whitespace() {
        i += 1;
    }
    head.get(i) == Some(&b'<')
}
