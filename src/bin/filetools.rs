//! `filetools` — CLI for the reversible file serialization format.
//!
//!   # extract -> envelope JSON (+ sidecar id-map next to it)
//!   filetools extract --input report.xml --out report.ft.json
//!
//!   # scan -> lightweight, paginated manifest to stdout (pick ids first)
//!   filetools scan --input big.xlsx --offset 0 --limit 100
//!
//!   # read -> hydrate only the blocks you selected (also paginated)
//!   filetools read --input big.xlsx --id 'sheet[0].rows[0-99]'
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

use filetools_rs::idmap::IdMap;
use filetools_rs::model::{BlockManifest, Envelope, GrepMatch, GrepOptions};
use filetools_rs::patch::Patch;
use serde::Serialize;

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
    /// Print a lightweight manifest (no content hydrated) as JSON to stdout.
    ///
    /// Use this to pick block ids before hydrating them with `read`.
    Scan {
        #[arg(long)]
        input: PathBuf,
        /// Skip the first N blocks.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Return at most N blocks. 0 means no limit.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// Hydrate specific blocks by id and print them as JSON to stdout.
    ///
    /// Scope with `--id` (repeatable). With no ids, every block is returned;
    /// use `--offset`/`--limit` to page through a dense file.
    Read {
        #[arg(long)]
        input: PathBuf,
        /// Block id to hydrate. Repeat to request several. Accepts structural
        /// paths, `part:<name>` markers, and xlsx `sheet[n].rows[a-b]` ranges.
        #[arg(long = "id")]
        ids: Vec<String>,
        /// Skip the first N blocks of the result.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Return at most N blocks. 0 means no limit.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// Search block text for a pattern and print matching ids as JSON.
    ///
    /// The discovery counterpart to `read`: locate the blocks you care about
    /// without hydrating the whole document, then feed the ids into `read` or
    /// `reconstruct`.
    Grep {
        #[arg(long)]
        input: PathBuf,
        /// Literal substring to search for (matched per line of block text).
        #[arg(long)]
        pattern: String,
        /// Case-insensitive matching.
        #[arg(long, default_value_t = false)]
        ignore_case: bool,
        /// Stop after N matches. 0 means no limit.
        #[arg(long, default_value_t = 0)]
        limit: usize,
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
        Cmd::Scan {
            input,
            offset,
            limit,
        } => cmd_scan(&input, offset, limit),
        Cmd::Read {
            input,
            ids,
            offset,
            limit,
        } => cmd_read(&input, &ids, offset, limit),
        Cmd::Grep {
            input,
            pattern,
            ignore_case,
            limit,
        } => cmd_grep(&input, &pattern, ignore_case, limit),
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
    let mut output = filetools_rs::extract(&path_str, &bytes)?;

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

fn cmd_scan(input: &Path, offset: usize, limit: usize) -> Result<()> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let path_str = input.to_string_lossy();
    let result = filetools_rs::scan(&path_str, &bytes)?;

    let total = result.blocks.len();
    let blocks = paginate(result.blocks, offset, limit);
    let returned = blocks.len();

    let view = ScanView {
        file_type: result.file_type,
        block_count: result.block_count,
        total_tokens: result.total_tokens,
        offset,
        returned,
        total,
        blocks,
    };
    print_json(&view)?;
    eprintln!(
        "scanned {} [{:?}, {} blocks, returned {}/{} from offset {}]",
        input.display(),
        view.file_type,
        view.block_count,
        returned,
        total,
        offset,
    );
    Ok(())
}

fn cmd_read(input: &Path, ids: &[String], offset: usize, limit: usize) -> Result<()> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let path_str = input.to_string_lossy();
    let nodes = filetools_rs::read(&path_str, &bytes, ids)?;

    let total = nodes.len();
    let nodes = paginate(nodes, offset, limit);
    let returned = nodes.len();

    let view = ReadView {
        offset,
        returned,
        total,
        nodes,
    };
    print_json(&view)?;
    eprintln!(
        "read {} [{} ids requested, returned {}/{} from offset {}]",
        input.display(),
        ids.len(),
        returned,
        total,
        offset,
    );
    Ok(())
}

fn cmd_grep(input: &Path, pattern: &str, ignore_case: bool, limit: usize) -> Result<()> {
    let bytes = fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let path_str = input.to_string_lossy();
    let opts = GrepOptions {
        ignore_case,
        limit: (limit > 0).then_some(limit),
    };
    let matches = filetools_rs::grep(&path_str, &bytes, pattern, &opts)?;

    let view = GrepView {
        pattern: pattern.to_string(),
        returned: matches.len(),
        matches,
    };
    print_json(&view)?;
    eprintln!(
        "grep {} [pattern {:?}, {} matches]",
        input.display(),
        pattern,
        view.returned,
    );
    Ok(())
}

/// Apply `offset`/`limit` paging to a vector. `limit == 0` means no cap.
fn paginate<T>(items: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    let mut it = items.into_iter().skip(offset);
    if limit == 0 {
        it.collect()
    } else {
        it.by_ref().take(limit).collect()
    }
}

/// Serialize `value` as pretty JSON to stdout.
fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{json}");
    Ok(())
}

/// Paginated manifest view printed by `scan`.
#[derive(Serialize)]
struct ScanView {
    file_type: filetools_rs::model::FileType,
    /// Total blocks in the document (before paging).
    block_count: usize,
    total_tokens: usize,
    /// Paging cursor used for this response.
    offset: usize,
    /// Number of blocks in `blocks`.
    returned: usize,
    /// Total blocks available to page through.
    total: usize,
    blocks: Vec<BlockManifest>,
}

/// Paginated hydration view printed by `read`.
#[derive(Serialize)]
struct ReadView {
    offset: usize,
    returned: usize,
    total: usize,
    nodes: Vec<filetools_rs::model::DocNode>,
}

/// Match list printed by `grep`.
#[derive(Serialize)]
struct GrepView {
    pattern: String,
    returned: usize,
    matches: Vec<GrepMatch>,
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

    let result = filetools_rs::write(&env, &idmap, &original, &patch)?;
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
    fn walk(nodes: &mut [filetools_rs::model::DocNode]) {
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
