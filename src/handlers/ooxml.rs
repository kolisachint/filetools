//! OOXML container handler (docx / xlsx / pptx).
//!
//! An OOXML file is a zip of XML parts. The lossless XML core already does the
//! hard work on a single part's bytes; this handler adds the container layer:
//!
//!   * extract — select the relevant part(s) for the format, run the XML
//!     extractor on each. Every node's id-map entry records which `part` its
//!     spans index into; `for_hash` binds to the whole container so drift
//!     detection still works.
//!   * reconstruct — route each patch op to its target node's part, splice the
//!     edits into that part's bytes, then repackage the zip with the changed
//!     parts replaced and **every other entry copied through with its original
//!     compressed bytes untouched** (`raw_copy_file`). Nothing outside the
//!     edited parts changes; a no-op patch returns the container byte-identical.
//!
//! Part selection per format:
//!   * docx — `word/document.xml`
//!   * xlsx — `xl/sharedStrings.xml` (the human-readable cell text), if present
//!   * pptx — every `ppt/slides/slideN.xml`
//!
//! Note: docx text lives in `w:t` runs and xlsx/pptx text in `a:t`/`t` runs, so
//! the editable nodes are those leaf text elements. A richer layer that merges
//! runs into paragraph-level text (preserving untouched runs) is future work;
//! the generic element view is already lossless. xlsx worksheets (numeric/ref
//! cells) and per-sheet inline strings are not yet surfaced.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

use anyhow::{bail, Context, Result};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use super::xml;
use crate::idmap::{verify_spans, IdMap, NodeLoc};
use crate::model::{Attr, DocNode, Fidelity};
use crate::patch::{self, Op, Patch};

/// A container handler. `select` chooses which entries to expose for editing,
/// given the container's full entry list.
pub struct OoxmlHandler {
    type_name: &'static str,
    select: fn(&[String]) -> Vec<String>,
}

/// docx: edits `word/document.xml`.
pub fn docx() -> OoxmlHandler {
    OoxmlHandler {
        type_name: "docx",
        select: |names| pick_exact(names, "word/document.xml"),
    }
}

/// xlsx: edits the shared-strings table (human-readable cell text).
pub fn xlsx() -> OoxmlHandler {
    OoxmlHandler {
        type_name: "xlsx",
        select: |names| pick_exact(names, "xl/sharedStrings.xml"),
    }
}

/// pptx: edits every slide part.
pub fn pptx() -> OoxmlHandler {
    OoxmlHandler {
        type_name: "pptx",
        select: |names| {
            let mut v: Vec<String> = names
                .iter()
                .filter(|n| {
                    n.starts_with("ppt/slides/slide")
                        && n.ends_with(".xml")
                        && !n.contains("/_rels/")
                })
                .cloned()
                .collect();
            v.sort();
            v
        },
    }
}

fn pick_exact(names: &[String], target: &str) -> Vec<String> {
    if names.iter().any(|n| n == target) {
        vec![target.to_string()]
    } else {
        Vec::new()
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
        let names = entry_names(bytes)?;
        let parts = (self.select)(&names);
        let multi = parts.len() > 1;

        let mut structure: Vec<DocNode> = Vec::new();
        let mut merged: BTreeMap<String, NodeLoc> = BTreeMap::new();

        for part in &parts {
            let part_bytes = read_part(bytes, part)?;
            let e = xml::extract(&part_bytes, for_hash, part)?;
            for (id, loc) in e.idmap.map {
                merged.insert(id, loc);
            }
            if multi {
                // Group each part's nodes under a synthetic, non-editable marker
                // so the LLM can tell slides apart. Marker ids are not in the
                // id-map, so they can't be patched.
                structure.push(DocNode {
                    id: format!("part:{part}"),
                    tag: "_part".to_string(),
                    attrs: vec![Attr {
                        name: "name".to_string(),
                        value: part.clone(),
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
        // Verify each part's nodes against that part's bytes.
        for (part, sub) in group_by_part(idmap) {
            let part_bytes = read_part(bytes, &part)?;
            verify_spans(&part_bytes, &sub)?;
        }
        Ok(())
    }

    fn reconstruct(&self, bytes: &[u8], idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>> {
        if patch.patch.is_empty() {
            return Ok(bytes.to_vec());
        }

        // Route each op to the part of the node it targets.
        let mut by_part: BTreeMap<String, Vec<Op>> = BTreeMap::new();
        for op in &patch.patch {
            let id = op
                .target_id()
                .with_context(|| "patch op has no resolvable target id")?;
            let loc = idmap
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("unknown node id `{id}`"))?;
            by_part
                .entry(loc.part.clone())
                .or_default()
                .push(op.clone());
        }

        // Apply per part. patch::apply validates guards and produces new bytes
        // in memory, so any failure aborts before the container is rebuilt.
        let mut new_parts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (part, ops) in by_part {
            if part.is_empty() {
                bail!("node has no part attribution; cannot reconstruct container");
            }
            let part_bytes = read_part(bytes, &part)?;
            let new = patch::apply(&part_bytes, idmap, &Patch { patch: ops })?;
            if new != part_bytes {
                new_parts.insert(part, new);
            }
        }

        if new_parts.is_empty() {
            return Ok(bytes.to_vec());
        }
        repackage(bytes, &new_parts)
    }
}

/// Split an id-map into one sub-map per part.
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

/// List entry names in the container, in archive order.
fn entry_names(container: &[u8]) -> Result<Vec<String>> {
    let zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    Ok(zip.file_names().map(|s| s.to_string()).collect())
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

/// Rebuild the container replacing the given parts, copying every other entry
/// through with its original compressed bytes preserved verbatim.
fn repackage(container: &[u8], new_parts: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(container))?;
    let mut out = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut out));
        for i in 0..zip.len() {
            let raw = zip.by_index_raw(i)?;
            let name = raw.name().to_string();
            if let Some(new_bytes) = new_parts.get(&name) {
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
