//! OOXML container handler (docx today; xlsx/pptx share the mechanism).
//!
//! An OOXML file is a zip of XML parts. The lossless XML core already does the
//! hard work on a single part's bytes; this handler adds the container layer:
//!
//!   * extract — pull the main part (`word/document.xml`), run the XML
//!     extractor on it. The id-map's spans are relative to that part; its
//!     `for_hash` binds to the whole container so drift detection still works.
//!   * reconstruct — splice the patch into the part's bytes, then repackage the
//!     zip with that one part replaced and **every other entry copied through
//!     with its original compressed bytes untouched** (`raw_copy_file`). So
//!     nothing outside the edited part changes.
//!
//! v1 edits the main document part only. xlsx (`xl/sharedStrings.xml` +
//! per-sheet parts) and pptx (per-slide parts) need multi-part addressing and
//! are a follow-up — the container plumbing here is shared.
//!
//! Note: docx text lives in `w:t` runs, so the editable nodes are `w:t`
//! elements. A richer layer that merges runs into paragraph-level text (while
//! preserving untouched runs) is future work; the generic element view is
//! already lossless.

use std::io::{Cursor, Read, Write};

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use super::xml;
use crate::idmap::{verify_spans, IdMap};
use crate::model::{DocNode, Fidelity};
use crate::patch::Patch;

/// A container handler editing a single named XML part.
pub struct OoxmlHandler {
    type_name: &'static str,
    main_part: &'static str,
}

/// docx: edits `word/document.xml`.
pub fn docx() -> OoxmlHandler {
    OoxmlHandler {
        type_name: "docx",
        main_part: "word/document.xml",
    }
}

impl super::Handler for OoxmlHandler {
    fn type_name(&self) -> &'static str {
        self.type_name
    }
    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }

    fn extract(&self, bytes: &[u8], for_hash: &str) -> Result<(Vec<DocNode>, Option<IdMap>)> {
        let part = read_part(bytes, self.main_part)?;
        let e = xml::extract(&part, for_hash)?;
        Ok((e.nodes, Some(e.idmap)))
    }

    fn verify(&self, bytes: &[u8], idmap: &IdMap) -> Result<()> {
        let part = read_part(bytes, self.main_part)?;
        verify_spans(&part, idmap)
    }

    fn reconstruct(&self, bytes: &[u8], idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>> {
        let part = read_part(bytes, self.main_part)?;
        let new_part = crate::patch::apply(&part, idmap, patch)?;
        if new_part == part {
            // No-op patch: hand back the original container untouched.
            return Ok(bytes.to_vec());
        }
        replace_part(bytes, self.main_part, &new_part)
    }
}

/// Read one entry's decompressed bytes from a zip container.
fn read_part(container: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    let mut f = zip
        .by_name(name)
        .with_context(|| format!("part `{name}` not found in container"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Rebuild the container replacing one part, copying every other entry through
/// with its original compressed bytes preserved verbatim.
fn replace_part(container: &[u8], name: &str, new_bytes: &[u8]) -> Result<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(container))?;
    let mut out = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut out));
        for i in 0..zip.len() {
            let raw = zip.by_index_raw(i)?;
            if raw.name() == name {
                let opts = SimpleFileOptions::default().compression_method(raw.compression());
                drop(raw);
                writer.start_file(name, opts)?;
                writer.write_all(new_bytes)?;
            } else {
                writer.raw_copy_file(raw)?;
            }
        }
        writer.finish()?;
    }
    Ok(out)
}
