# filetools — `hoo-extract`

A reversible, token-efficient file serialization format for LLMs.

Extract a file to compact semantic JSON (an **envelope**), let an LLM edit
nodes and return a patch, then reconstruct the original format **losslessly** —
every byte the LLM didn't touch is reproduced exactly.

The trick: the JSON is a *projection* of the file, not a re-encoding of it. The
original stays the source of truth. A sidecar **id-map** records the exact byte
spans each node occupies, so reconstruction splices edits into those spans and
copies everything else through verbatim. No round-trip through a lossy AST.

## Status

Working vertical slice:

| Capability | State |
|---|---|
| Generic XML (lossless, in-place byte-splice) | ✅ |
| drawio (thin semantic layer over XML) | ✅ (uncompressed) |
| Sidecar id-map + content-addressed ids | ✅ |
| RFC-6902-style patches (id-based pointers) | ✅ |
| Hash-guarded ops, atomic all-or-nothing apply | ✅ |
| Verify-on-extract (span/hash self-check) | ✅ |
| Source-hash drift detection on reconstruct | ✅ |
| Read-only fallback for unknown binary | ✅ |
| OOXML (docx/xlsx/pptx) | ⏳ next — builds on the XML core |
| PDF (in-place content patching) | ⏳ planned |
| drawio compressed `<diagram>` inflate | ⏳ planned |

## Usage

```bash
# Extract -> envelope JSON (+ sidecar id-map written alongside)
hoo-extract extract --input report.xml --out report.hoo.json

# Reconstruct: apply a patch back into the original format
hoo-extract reconstruct --envelope report.hoo.json --patch patch.json \
                        --out report_v2.xml

# Read-only view: strip ids, no sidecar, max token savings
hoo-extract extract --input data.bin --readonly
```

## Envelope

What the LLM sees. `id`s are stable, content-addressed, and used to address
edits.

```json
{
  "version": "1.0",
  "source": { "path": "report.xml", "type": "xml", "hash": "sha256:79ab…" },
  "fidelity": "lossless",
  "writable": true,
  "idmap_ref": "report.xml.idmap.json",
  "structure": [
    { "id": "el_8694f8af", "tag": "p",
      "attrs": [{ "name": "id", "value": "p1" }],
      "text": "Revenue grew 12%." }
  ]
}
```

The **sidecar** (never shown to the LLM) maps each id to byte spans + a guard
hash, and is bound to the original by hash:

```json
{
  "for_hash": "sha256:79ab…",
  "map": {
    "el_8694f8af": {
      "tag": "p",
      "element": { "start": 41, "end": 75 },
      "inner":   { "start": 53, "end": 70 },
      "attrs":   { "id": { "start": 48, "end": 50 } },
      "hash": "sha256:…"
    }
  }
}
```

## Patch format

RFC 6902 op vocabulary, adapted:

- **id-based pointers** — `/structure/<id>/text`, `/structure/<id>/attrs/<name>`
  (resolved through the id-map, not array indices, so they survive edits).
- **`add`** anchored to a stable neighbour via `after` / `before`.
- **`test`** carries a content `hash` (optimistic concurrency guard).
- **atomic** — any failed op or stale guard aborts the whole patch; the
  original is left untouched.

```json
{ "patch": [
  { "op": "test",    "path": "/structure/el_8694f8af", "hash": "sha256:…" },
  { "op": "replace", "path": "/structure/el_8694f8af/text", "value": "Revenue grew 18%." },
  { "op": "add",     "after": "el_8694f8af",
    "value": { "tag": "p", "attrs": [{"name":"id","value":"p2"}], "text": "See appendix A." } },
  { "op": "remove",  "path": "/structure/el_old" }
] }
```

## Fidelity model

Each handler declares what it can promise:

- **`lossless`** (xml, drawio) — untouched bytes reproduced exactly;
  verify-on-extract enforces span correctness before output is trusted.
- **`in_place_text`** (pdf, planned) — surgical text edits only; edits that
  don't fit the existing layout are rejected.
- **`read_only`** (unknown binary) — best-effort text extraction,
  `writable: false`, reconstruct refused.

## Known v1 limitations

- Text-replace is offered only for elements with a single contiguous text run
  (no mixed content) — keeps edits unambiguous and lossless.
- Inserted elements carry plain text only (no inline formatting yet).
- `add` inserts exactly at the anchor's byte boundary, so inserted nodes are
  not auto-indented.
- drawio handles the uncompressed form; compressed `<diagram>` payloads aren't
  inflated yet.

## Build & test

```bash
cargo build
cargo test
```
