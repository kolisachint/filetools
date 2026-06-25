//! filetools / `hoo-extract`: a reversible, token-efficient file serialization
//! format for LLMs.
//!
//! Extract a file to compact semantic JSON (the *envelope*) plus a sidecar
//! id-map. An LLM edits nodes and returns an RFC-6902-style patch. Reconstruct
//! applies the patch by splicing edits into the original's byte spans, leaving
//! all untouched content byte-for-byte intact.
//!
//! Design guarantees, by handler fidelity:
//!   * `Lossless`   (xml, drawio): untouched bytes reproduced exactly;
//!     verify-on-extract enforces span correctness before output is trusted.
//!   * `InPlaceText` (pdf, planned): surgical text edits only.
//!   * `ReadOnly`   (unknown binary): extract-only, `writable: false`.

pub mod handlers;
pub mod idmap;
pub mod model;
pub mod patch;

use anyhow::{bail, Context, Result};

use idmap::{sha256_hex, IdMap};
use model::{Envelope, Fidelity, Source};
use patch::Patch;

/// Outcome of extracting a file.
pub struct ExtractOutput {
    pub envelope: Envelope,
    /// Present for writable (id-map-bearing) formats.
    pub idmap: Option<IdMap>,
}

/// Extract `bytes` (originating from `path`) into an envelope + sidecar.
///
/// For `Lossless` handlers this runs verify-on-extract: it confirms every
/// recorded span actually points at its element and that hashes recompute, so
/// a downstream reconstruct can be trusted to be byte-faithful.
pub fn extract(path: &str, bytes: &[u8]) -> Result<ExtractOutput> {
    let hash = sha256_hex(bytes);
    let handler = handlers::for_path(path, bytes);
    let fidelity = handler.fidelity();
    let type_name = handler.type_name();

    let (structure, idmap) = handler.extract(bytes, &hash)?;

    if fidelity == Fidelity::Lossless {
        let map = idmap
            .as_ref()
            .context("lossless handler produced no id-map")?;
        if map.for_hash != hash {
            bail!("id-map is bound to a different original");
        }
        handler
            .verify(bytes, map)
            .context("verify-on-extract failed: handler is not byte-faithful for this input")?;
    }

    let idmap_ref = idmap
        .as_ref()
        .map(|_| format!("{}.idmap.json", file_name(path)));
    let envelope = Envelope {
        version: "1.0".to_string(),
        source: Source {
            path: path.to_string(),
            r#type: type_name.to_string(),
            hash,
        },
        fidelity,
        writable: idmap.is_some(),
        idmap_ref,
        structure,
    };
    Ok(ExtractOutput { envelope, idmap })
}

/// Apply a patch to the original and return the reconstructed bytes.
///
/// Verifies the original still matches the hash the envelope was extracted from
/// (fails loud on drift), refuses non-writable envelopes, and confirms the
/// sidecar belongs to this original before splicing.
pub fn reconstruct(
    envelope: &Envelope,
    idmap: &IdMap,
    original: &[u8],
    patch: &Patch,
) -> Result<Vec<u8>> {
    if !envelope.writable {
        bail!(
            "envelope is read-only (fidelity {:?}); cannot reconstruct",
            envelope.fidelity
        );
    }
    let actual = sha256_hex(original);
    if actual != envelope.source.hash {
        bail!(
            "original has drifted since extract: envelope expected {}, file is {}",
            envelope.source.hash,
            actual
        );
    }
    if idmap.for_hash != envelope.source.hash {
        bail!("sidecar id-map does not match this original (hash mismatch)");
    }
    let handler = handlers::for_type(&envelope.source.r#type)
        .with_context(|| format!("no handler for type `{}`", envelope.source.r#type))?;
    handler.reconstruct(original, idmap, patch)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
