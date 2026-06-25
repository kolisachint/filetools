//! `filetools` — CLI for the reversible file serialization format.
//!
//!   # extract -> envelope JSON (+ sidecar id-map next to it)
//!   filetools extract --input report.xml --out report.ft.json
//!
//!   # reconstruct: apply a patch back into the original format
//!   filetools reconstruct --envelope report.ft.json --patch patch.json \
//!                         --out report_v2.xml
//!
//!   # read-only view (no ids, max token savings)
//!   filetools extract --input data.bin --readonly

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use filetools::idmap::IdMap;
use filetools::model::Envelope;
use filetools::patch::Patch;

#[derive(Parser)]
#[command(
    name = "filetools",
    about = "Reversible, token-efficient file serialization for LLMs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Extract a file to a semantic JSON envelope (+ sidecar id-map).
    Extract {
        #[arg(long)]
        input: PathBuf,
        /// Envelope output path. Defaults to `<input>.ft.json`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Strip ids and skip the sidecar — analysis-only, max token savings.
        #[arg(long)]
        readonly: bool,
    },
    /// Apply a patch to the original and write the reconstructed file.
    Reconstruct {
        /// The envelope produced by `extract`.
        #[arg(long)]
        envelope: PathBuf,
        /// The patch JSON (`{ "patch": [...] }`).
        #[arg(long)]
        patch: PathBuf,
        /// Output path for the reconstructed file.
        #[arg(long)]
        out: PathBuf,
        /// Original source file. Defaults to the path recorded in the envelope.
        #[arg(long)]
        original: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Extract {
            input,
            out,
            readonly,
        } => cmd_extract(&input, out.as_deref(), readonly),
        Cmd::Reconstruct {
            envelope,
            patch,
            out,
            original,
        } => cmd_reconstruct(&envelope, &patch, &out, original.as_deref()),
    }
}

fn cmd_extract(input: &Path, out: Option<&Path>, readonly: bool) -> Result<()> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let path_str = input.to_string_lossy();
    let mut output = filetools::extract(&path_str, &bytes)?;

    let envelope_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| with_suffix(input, ".ft.json"));

    if readonly {
        // Strip ids for analysis: smaller, but not reconstructable.
        strip_ids(&mut output.envelope);
        output.envelope.writable = false;
        output.envelope.idmap_ref = None;
        output.idmap = None;
    }

    fs::write(&envelope_path, serde_json::to_vec_pretty(&output.envelope)?)
        .with_context(|| format!("writing {}", envelope_path.display()))?;

    if let Some(idmap) = &output.idmap {
        let sidecar = sidecar_path(&envelope_path, &output.envelope);
        fs::write(&sidecar, serde_json::to_vec_pretty(idmap)?)
            .with_context(|| format!("writing {}", sidecar.display()))?;
        eprintln!(
            "extracted {} -> {} (+ {})  [{:?}, {} nodes]",
            input.display(),
            envelope_path.display(),
            sidecar.display(),
            output.envelope.fidelity,
            output.idmap.as_ref().map(|m| m.map.len()).unwrap_or(0),
        );
    } else {
        eprintln!(
            "extracted {} -> {}  [{:?}, read-only]",
            input.display(),
            envelope_path.display(),
            output.envelope.fidelity,
        );
    }
    Ok(())
}

fn cmd_reconstruct(
    envelope: &Path,
    patch: &Path,
    out: &Path,
    original: Option<&Path>,
) -> Result<()> {
    let env: Envelope = serde_json::from_slice(&fs::read(envelope)?)
        .with_context(|| format!("parsing envelope {}", envelope.display()))?;

    let idmap_ref = env
        .idmap_ref
        .as_ref()
        .context("envelope has no idmap_ref — it is read-only and cannot be reconstructed")?;
    let sidecar = envelope.parent().unwrap_or(Path::new(".")).join(idmap_ref);
    let idmap: IdMap = serde_json::from_slice(
        &fs::read(&sidecar).with_context(|| format!("reading sidecar {}", sidecar.display()))?,
    )
    .with_context(|| format!("parsing sidecar {}", sidecar.display()))?;

    let original_path = original
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&env.source.path));
    let original = fs::read(&original_path)
        .with_context(|| format!("reading original {}", original_path.display()))?;

    let patch: Patch = serde_json::from_slice(&fs::read(patch)?)
        .with_context(|| format!("parsing patch {}", patch.display()))?;

    let result = filetools::reconstruct(&env, &idmap, &original, &patch)?;
    fs::write(out, &result).with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "reconstructed {} -> {} ({} ops, {} bytes)",
        original_path.display(),
        out.display(),
        patch.patch.len(),
        result.len(),
    );
    Ok(())
}

fn strip_ids(env: &mut Envelope) {
    fn walk(nodes: &mut [filetools::model::DocNode]) {
        for n in nodes {
            n.id.clear();
            walk(&mut n.children);
        }
    }
    walk(&mut env.structure);
}

/// `report.xml` + `.ft.json` -> `report.ft.json`.
fn with_suffix(input: &Path, suffix: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = input.parent().unwrap_or(Path::new("."));
    parent.join(format!("{stem}{suffix}"))
}

/// Sidecar lives next to the envelope, named per the envelope's `idmap_ref`.
fn sidecar_path(envelope_path: &Path, env: &Envelope) -> PathBuf {
    let dir = envelope_path.parent().unwrap_or(Path::new("."));
    match &env.idmap_ref {
        Some(name) => dir.join(name),
        None => dir.join("idmap.json"),
    }
}
