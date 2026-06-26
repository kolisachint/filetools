# filetools documentation

Reversible, token-efficient file serialization for LLMs: extract a file to
compact semantic JSON, edit nodes, and reconstruct the original format
losslessly — every byte the edit didn't touch is reproduced exactly.

## Contents

- [Product overview](product.md) — what it is, who it's for, the core idea.
- [Install](install.md) — install the CLI and add the library as a dependency.
- [CLI usage](usage.md) — `extract`, `scan`, `read`, `grep`, `reconstruct`.
- [Formats](formats.md) — per-format fidelity (XML, OOXML, drawio, PDF, CSV,
  HTML, and read-only scanners).
- [Patch format](patch-format.md) — the op vocabulary and pointer scheme.
- [Drift recovery](drift-recovery.md) — autonomous recovery when the original
  is rewritten out-of-band.
- [Grep](grep.md) — locate blocks by text without hydrating the document.

## The 30-second version

```bash
filetools extract --input report.docx          # -> report.docx.ft.json (+ sidecar)
filetools grep    --input report.docx --pattern "Q1 revenue"
filetools read    --input report.docx --id paragraph[3]
filetools reconstruct --envelope report.docx.ft.json --patch patch.json \
                      --out report_v2.docx
```

The JSON is a *projection* of the file, not a re-encoding. The original stays
the source of truth; a sidecar id-map records the byte spans each node occupies,
so reconstruction splices edits into those spans and copies everything else
through verbatim.
