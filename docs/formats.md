# Formats

Each handler declares a **fidelity** that states what it can promise:

- **`lossless`** (xml, drawio, OOXML, csv, html) — untouched bytes reproduced
  exactly; verify-on-extract enforces span correctness before output is trusted.
- **`in_place_text`** (pdf) — layout-preserving text replacement only; glyph
  positions are untouched and there is no reflow.
- **`read_only`** (unknown binary, and the lightweight scanners) — best-effort
  text/metadata extraction, `writable: false`, reconstruct refused.

## Support matrix

| Capability | State |
|---|---|
| Generic XML (lossless, in-place byte-splice) | yes |
| drawio (semantic layer over XML, compressed + uncompressed) | yes |
| docx (edits `word/document.xml`) | yes |
| xlsx (shared strings + worksheet cells) | yes |
| pptx (every `ppt/slides/slideN.xml`, multi-part, atomic) | yes |
| Run-merging text layer (paragraph = one editable string) | yes |
| PDF text replacement (layout-preserving) | yes |
| CSV (cell-addressable, byte-faithful field edits) | yes |
| HTML (span-tracking tokenizer, surgical text edits) | yes |
| Sidecar id-map + content-addressed ids | yes |
| RFC-6902-style patches (id-based pointers) | yes |
| Hash-guarded ops, atomic all-or-nothing apply | yes |
| Verify-on-extract (span/hash self-check) | yes |
| Autonomous drift recovery on reconstruct | yes |
| Read-only fallback for unknown binary | yes |
| Lightweight scanners (svg, markdown, mermaid, zip, image metadata) | scan-only |

## XML (`lossless`)

Plain XML is handled as a whole-file byte stream. Every element becomes a node
with a byte span; edits splice into the span and everything else is copied
through verbatim. This is the core the other handlers build on.

## OOXML — docx / xlsx / pptx (`lossless`)

An OOXML file is a zip of XML parts. The handler selects the relevant part(s)
per format, runs the lossless XML core on each, and records which `part` every
node's spans index into. On reconstruct it routes each patch op to its target
node's part, splices the edits, and repackages the zip with the changed parts
replaced — **every other entry is copied through with its original compressed
bytes untouched**. Edits spanning multiple parts (e.g. several pptx slides in
one patch) are applied atomically: if any part fails a guard, the container is
not rebuilt. A no-op patch returns the container byte-identical.

Part selection:

- **docx** → `word/document.xml`
- **xlsx** → `xl/sharedStrings.xml` (human-readable cell text) **and**
  `xl/worksheets/sheetN.xml` (cell values `<v>` and inline strings `<t>`)
- **pptx** → every `ppt/slides/slideN.xml` (each wrapped under a synthetic,
  non-editable `_part` marker so slides stay distinguishable)

**Run merging.** OOXML splits a paragraph's text across runs (`w:r`/`w:t`), so
`Q1 ` and `revenue grew.` may be separate runs. The handler collapses each
paragraph (`w:p` / `a:p` / xlsx `si`) into a single editable string and hides
the runs, so the agent sees `"Q1 revenue grew."` not the run markup. On a
text-replace it diffs old against new, places the change into the run(s) it
actually touches, and leaves every other run **byte-identical** — so formatting
on untouched runs (bold, colour, etc.) is preserved exactly.

Note a shared-string cell's `<v>` is an *index* into the strings table — edit
the text via `xl/sharedStrings.xml`, not the index.

## drawio (`lossless`, compressed and uncompressed)

A bare `<mxGraphModel>` file is plain whole-file XML. A real `<mxfile>` holds
one or more `<diagram>` parts, each storing the model either inline or — by
default — **compressed** as `base64(deflateRaw(encodeURIComponent(xml)))`. Each
diagram is treated as a part whose editable stream is the decoded inner XML:
edits are spliced into the decoded XML, the diagram is re-encoded, and the new
blob is spliced into the outer file. **Untouched diagrams keep their original
blob byte-for-byte**; only edited diagrams are recompressed (deflate output
isn't byte-stable, but the decoded XML differs only in the edited spans). The
codec is verified against real drawio output, not just itself.

## PDF (`in_place_text`)

PDF has no document tree: text is drawn by operators (`Tj`, `TJ`, `'`, `"`)
inside compressed content streams. The handler (via `lopdf`) decodes each page's
content stream and exposes the literal strings as editable text nodes. A
text-replace substitutes the string and re-encodes the content stream, leaving
every glyph position untouched — a **layout-preserving, text-level edit, never a
reflow**. Text ids are derived from page/string position and recomputed on both
sides, so nothing positional is persisted; hash guards work against the current
string bytes.

Limits: text `replace` only (no `add`/`remove`/`attr`). Strings are treated as
Latin-1/ASCII; documents with custom font encodings or `ToUnicode` maps may not
round-trip non-ASCII text. Length-changing edits are allowed but, with no
reflow, a much longer string can overrun its box. Reconstruct re-serialises the
document (all original objects retained) rather than appending an incremental
update.

## CSV (`lossless`)

Cells are addressable, byte-faithful fields. Edits replace a field's bytes
without disturbing quoting, delimiters, or line endings elsewhere.

## HTML (`lossless`)

A span-tracking tokenizer exposes headings, title, and paragraph text as
editable nodes and splices replacements surgically, leaving surrounding markup
untouched.

## Read-only scanners

svg, markdown, mermaid, zip, and image (jpg/png) inputs have lightweight
manifest scanners for `scan`/`read` discovery. They are `writable: false`:
locate content, but reconstruct is refused. Unknown binaries fall back to
best-effort text extraction, also read-only.

## Known limitations

- Text-replace is offered only for elements with a single contiguous text run
  (no mixed content) — keeps edits unambiguous and lossless.
- Inserted elements carry plain text only (no inline formatting yet).
- `add` inserts exactly at the anchor's byte boundary, so inserted nodes are
  not auto-indented.
