# Development Rules

## Repo Map

`filetools` is a single Rust crate: a binary CLI and a library.

Code:

- `src/lib.rs` — library root: `extract`, `scan`, `read`, `grep`, `edit`,
  `write` public API; autonomous drift recovery on reconstruct
- `src/bin/filetools.rs` — CLI entry point (clap-based)
- `src/model.rs` — data model (envelope, id-map, patch types, scan result)
- `src/idmap.rs` — sidecar id-map: content-addressed byte-span tracking
- `src/patch.rs` — RFC-6902-style patch application
- `src/cache.rs` — bounded LRU used by the in-process scan/extract caches
- `src/handlers/mod.rs` — handler registry
- `src/handlers/xml.rs` — generic XML (lossless byte-splice)
- `src/handlers/ooxml.rs` — OOXML: docx, xlsx, pptx
- `src/handlers/xlsx.rs` — hierarchical XLSX scan + lazy row-range read
- `src/handlers/drawio.rs` — drawio (compressed/uncompressed)
- `src/handlers/pdf.rs` — PDF text replacement
- `src/handlers/html.rs` — HTML span-tracking tokenizer + surgical text edits
- `src/handlers/csv.rs` — CSV cell-addressable, byte-faithful field edits
- `src/handlers/optimized.rs` — lightweight manifest scanners (pptx, pdf, svg,
  drawio, markdown, mermaid, csv, html, zip, image metadata)
- `src/handlers/readonly.rs` — read-only fallback for unknown binaries
- `tests/roundtrip.rs` — integration tests
- `tests/scan_formats.rs` — scan-level tests for lightweight handlers
- `tests/edit_formats.rs` — CSV/HTML edit/write round-trip tests
- `.github/workflows/` — `ci.yml`, `release.yml`
- `.agents/commands/` — slash-command definitions (`pr.md`)
- `docs/` — product overview, install, CLI usage, formats, patch format,
  drift recovery, grep

## API Layers

filetools exposes two workflows: a manifest-first path for token-sensitive agent loops, and a full-content path for batch operations.

### Manifest-First Workflow (Agent-Optimized)

Use this when the caller needs to select specific blocks before loading content.
Signatures below match `src/lib.rs`; see `docs/` for prose.

```rust
// 1. Get lightweight manifest (no content hydrated)
pub fn scan(path: &str, bytes: &[u8]) -> Result<ScanResult>

// 1b. (optional) Locate blocks by text without hydrating the document.
//     Each GrepMatch.block_id resolves directly via read/reconstruct.
pub fn grep(path: &str, bytes: &[u8], needle: &str, opts: &GrepOptions)
    -> Result<Vec<GrepMatch>>

// 2. Select block IDs from manifest/grep, then hydrate only those
//    (empty ids => hydrate everything).
pub fn read(path: &str, bytes: &[u8], ids: &[String]) -> Result<Vec<DocNode>>

// 3. Apply a patch and return reconstructed bytes (does not touch disk)
pub fn edit(envelope: &Envelope, idmap: &IdMap, original: &[u8], patch: &Patch)
    -> Result<Vec<u8>>

// 4. Same as edit; the public name used to persist a reconstruction
pub fn write(envelope: &Envelope, idmap: &IdMap, original: &[u8], patch: &Patch)
    -> Result<Vec<u8>>
```

`extract(path, bytes) -> ExtractOutput { envelope, idmap }` is the entry that
produces the `Envelope` + sidecar `IdMap` that `edit`/`write` consume.

**Drift recovery (autonomous).** If `original` no longer matches the hash the
envelope was extracted from (e.g. openpyxl rewrote the xlsx out-of-band),
`edit`/`write` do **not** fail. They re-extract the current bytes, re-target
each op by a parent-anchored *semantic* fingerprint (normalized text, not byte
hash), and apply atomically against the fresh id-map. Recovery emits a one-line
note to stderr. If any target is gone or ambiguous after the rewrite, the whole
patch is refused (nothing is written). See `docs/drift-recovery.md`.

### Data Structures

**ScanResult** — returned by `scan()`, contains document metadata without content:

```rust
pub struct ScanResult {
    pub file_type: FileType,
    pub block_count: usize,
    pub total_tokens: usize,
    pub blocks: Vec<BlockManifest>,
}
```

**BlockManifest** — per-block metadata for selection:

```rust
pub struct BlockManifest {
    pub id: String,              // Structural path: section[2].table[0].row[3]
    pub block_type: BlockType,   // Heading, Paragraph, Table, List, Code, Image, Cell
    pub preview: String,         // First ~100 chars for disambiguation
    pub content_hash: String,    // Staleness guard
    pub parent_id: Option<String>,
    pub token_estimate: usize,
    pub section_name: String,
    pub section_number: usize,
}
```

### Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Block ID scheme | Structural path | Stable across non-destructive edits |
| Manifest fields | All (id, type, preview, hash, parent, tokens, name, number) | Self-describing, cheap to compute |
| DocScan output | No file_name | Caller already has the path |
| DocRead options | IDs only | Neighbors can be added later |
| Patch guard | Optional `expected: Option<String>` | Skip check when safe, enforce when needed |
| Manifest persistence | In-memory cache (HashMap) | Fast for single-session, no file pollution |
| First format | DOCX | Most common binary target |

### Block ID Scheme

Block IDs are structural paths derived from document hierarchy:

- `section[0]` — first section
- `section[0].paragraph[1]` — second paragraph in first section
- `section[2].table[0].row[3]` — fourth row of first table in third section

Content hash is stored separately as `content_hash` for staleness detection, not as the identity.

> Note: `BlockManifest.content_hash` is a manifest-display field and is not the
> guard the engine enforces. Concurrency/staleness is enforced two ways: an
> explicit `test` op (`Op::Test { path, hash }`, validated against the
> id-map's per-node `hash`), and autonomous parent-anchored drift recovery on
> reconstruct (semantic fingerprint, see `docs/drift-recovery.md`). The real
> per-node content hash lives in the sidecar id-map (`NodeLoc.hash`).

### Patch Operations

Patches are an RFC-6902-style op list over id-based pointers. The actual wire
type is `patch::Op` (see `src/patch.rs`):

```rust
pub enum Op {
    Test    { path: String, hash: String },         // optimistic guard
    Replace { path: String, value: String },        // text or attr value
    Remove  { path: String },                        // delete element
    Add     { after: Option<String>, before: Option<String>, value: NewElement },
}
```

Pointers are `/structure/<id>/text`, `/structure/<id>/attrs/<name>`, or
`/structure/<id>`. A failed `test` (hash mismatch) or any failed op aborts the
whole patch atomically; the original is left untouched.

## Conversational Style

- Keep answers short and concise
- No emojis in commits, issues, PR comments, or code
- No fluff or cheerful filler text
- Technical prose only, be kind but direct

## Code Quality

- Read files in full before making wide-ranging changes, before editing files
  you have not already fully inspected, and when asked to investigate or audit.
  Do not rely only on search snippets for broad changes.
- Match the surrounding style: import order, naming, error handling (`anyhow`
  for the binary, typed results in libraries)
- Avoid `unwrap()`/`expect()` outside tests; thread errors with `?`
- Do not preserve backward compatibility unless the user explicitly asks
- Always ask before removing functionality that appears intentional

## Commands

- After code changes (not doc-only changes), run all three and fix everything
  before committing:
  ```bash
  cargo fmt --all --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
  ```
- If you create or modify a test, run it and iterate until it passes
- NEVER commit unless the user asks

## Slash Commands

- `/pr [patch|minor|major]` — opens a release PR on a feature branch and labels
  it `cargo:<bump>` so `release.yml` bumps the version and publishes on merge.
  Defined in `.agents/commands/pr.md`. Defaults to `patch`.
- Slash-command definitions live in `.agents/commands/`.

## Releasing

**Version semantics**:

- `patch` — bug fixes and additions
- `minor` — API changes
- `major` — large breaking changes

### Flow (do NOT bump versions or tag by hand)

**Never edit `version = "…"` in `Cargo.toml` inside a feature PR.** The release
workflow is the sole owner of the version: it computes the next version from the
latest `v*` git tag plus the PR's `cargo:<bump>` label, then rewrites the
manifest. A manual bump is at best ignored and at worst confusing.

1. `/pr <bump>` opens a PR labeled `cargo:<bump>`.
2. On merge, `release.yml` derives the next version from the latest `v*` tag,
   bumps `Cargo.toml`, updates `Cargo.lock`, commits `release: v<version>`,
   tags `v<version>`, and pushes `main`.
3. The release workflow publishes to crates.io and creates a GitHub release.

Secrets required: `CRATES_IO_TOKEN` (crates.io publish). `GITHUB_TOKEN` is
provided automatically.

Manual fallback (only if asked): `git tag vX.Y.Z && git push origin vX.Y.Z`.

## **CRITICAL** Git Rules for Parallel Agents **CRITICAL**

Multiple agents may work on different files in the same worktree simultaneously.

### Committing

- ONLY commit files YOU changed in THIS session
- Include `fixes #<number>` / `closes #<number>` when there is a related issue/PR
- NEVER use `git add -A` or `git add .` — these sweep up other agents' changes
- ALWAYS `git add <specific-file-paths>` listing only files you modified
- Run `git status` before committing and verify you are staging only YOUR files

### Forbidden Git Operations

These can destroy other agents' work and are never allowed:

- `git reset --hard`
- `git checkout .`
- `git clean -fd`
- `git stash`
- `git add -A` / `git add .`
- `git commit --no-verify`

### Safe Workflow

```bash
git status                      # 1. check first
git add src/handlers/xml.rs     # 2. stage only your files
git commit -m "fix(xml): ..."   # 3. commit
git pull --rebase && git push   # 4. push (never reset/checkout)
```

### If Rebase Conflicts Occur

- Resolve conflicts in YOUR files only
- If a conflict is in a file you did not modify, abort and ask the user
- NEVER force push over shared history

### User Override

If the user's instructions conflict with these rules, ask for confirmation that
they want to override. Only then proceed.
