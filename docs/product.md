# Product overview

## What it is

filetools turns a file into compact, editable JSON (an **envelope**), lets an
agent edit nodes and return a patch, then reconstructs the original format
**losslessly** — every byte the agent didn't touch is reproduced exactly.

The trick: the JSON is a *projection* of the file, not a re-encoding of it. The
original stays the source of truth. A sidecar **id-map** records the exact byte
spans each node occupies, so reconstruction splices edits into those spans and
copies everything else through verbatim. No round-trip through a lossy AST.

## Who it's for

- **LLM/agent tool authors** who need to read and edit real-world documents
  (docx, xlsx, pptx, PDF, drawio, XML, CSV, HTML) without burning tokens on the
  full file and without corrupting the parts they didn't mean to change.
- **Pipelines** that mutate documents programmatically and need a byte-faithful
  guarantee on untouched content.

## Why it exists

Two problems with naively feeding a document to an LLM:

1. **Tokens.** A whole spreadsheet or deck is enormous. filetools offers a
   manifest-first loop: `scan` (cheap structure), `grep` (locate by text), then
   `read` only the blocks you need.
2. **Fidelity.** Re-serializing through a generic library reflows whitespace,
   reorders attributes, and drops formatting. filetools never re-encodes
   untouched content — it byte-splices edits into the original.

## Core concepts

| Concept | Meaning |
|---|---|
| **Envelope** | The JSON projection the agent sees: `source`, `fidelity`, `writable`, and a `structure` tree of nodes with stable ids. |
| **Id-map (sidecar)** | Never shown to the agent. Maps each node id to byte spans + a guard hash, bound to the original by content hash. |
| **Node id** | Stable, content-addressed handle (`el_8694f8af`) used to address edits. |
| **Patch** | RFC-6902-style op list over id-based pointers (`test`/`replace`/`add`/`remove`). |
| **Fidelity** | What a handler can promise: `lossless`, `in_place_text`, or `read_only`. |

## Workflows

**Manifest-first (token-sensitive agent loop):**
`extract`/`scan` → `grep`/`read` selected ids → patch → `reconstruct`.

**Batch (whole document):**
`extract` everything → patch → `reconstruct`.

## Guarantees

- Untouched bytes are reproduced exactly for `lossless` handlers
  (verify-on-extract proves span correctness before output is trusted).
- Patches are **atomic**: any failed op or stale guard aborts the whole patch;
  the original is left untouched.
- If the original is rewritten out-of-band between extract and reconstruct,
  recovery is **autonomous** (re-target by semantic fingerprint) or it
  **refuses** — never a silent mis-edit. See [drift recovery](drift-recovery.md).
