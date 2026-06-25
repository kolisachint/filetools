//! Format handlers and dispatch.

pub mod drawio;
pub mod readonly;
pub mod xml;

use crate::idmap::IdMap;
use crate::model::{DocNode, Fidelity};

/// A format handler. Every handler resolves nodes back to byte spans (via the
/// id-map) except read-only ones, which extract text for context only.
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
}

/// Pick a handler from the file extension, falling back to a content sniff and
/// finally to best-effort read-only text extraction.
pub fn for_path(path: &str, bytes: &[u8]) -> Box<dyn Handler> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
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

fn looks_like_xml(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(256)];
    let mut i = 0;
    while i < head.len() && head[i].is_ascii_whitespace() {
        i += 1;
    }
    head.get(i) == Some(&b'<')
}
