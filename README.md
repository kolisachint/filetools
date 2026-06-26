# filetools

Reversible, token-efficient file serialization for LLMs. The `filetools` CLI
extracts a file to editable JSON and reconstructs it after edits — **losslessly**.

Extract a file to compact semantic JSON (an **envelope**), let an LLM edit nodes
and return a patch, then reconstruct the original format with every byte the LLM
didn't touch reproduced exactly. The JSON is a *projection* of the file, not a
re-encoding: a sidecar **id-map** records the byte spans each node occupies, so
reconstruction splices edits into those spans and copies everything else through
verbatim. No round-trip through a lossy AST.

## Install

```bash
cargo install filetools-rs      # CLI
cargo add filetools-rs          # library
```

See [docs/install.md](docs/install.md).

## Quick start

```bash
filetools extract --input report.docx                   # -> report.docx.ft.json (+ sidecar)
filetools grep    --input report.docx --pattern "Q1 revenue"
filetools read    --input report.docx --id paragraph[3]
filetools reconstruct --envelope report.docx.ft.json --patch patch.json \
                      --out report_v2.docx
```

## Highlights

- **Lossless** byte-splice for XML, OOXML (docx/xlsx/pptx), drawio, CSV, HTML;
  layout-preserving text edits for PDF.
- **Token-sensitive loop**: `scan` structure, `grep` by text, `read` only the
  blocks you need.
- **Atomic** patches with hash guards; any failure leaves the original untouched.
- **Autonomous drift recovery**: if the file is rewritten out-of-band between
  extract and reconstruct, edits are re-targeted by semantic fingerprint — or
  refused, never mis-applied.

## Documentation

Full docs live in [`docs/`](docs/README.md):

- [Product overview](docs/product.md)
- [Install](docs/install.md)
- [CLI usage](docs/usage.md)
- [Formats & fidelity](docs/formats.md)
- [Patch format](docs/patch-format.md)
- [Drift recovery](docs/drift-recovery.md)
- [Grep](docs/grep.md)

## Build & test

```bash
cargo build
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
