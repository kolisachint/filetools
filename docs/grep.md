# Grep

`grep` is the discovery counterpart to `read`: locate the blocks you care about
by text, without hydrating the whole document. Each match carries a `block_id`
that resolves directly via `read` or a patch, so you funnel matches straight
into an edit.

## CLI

```bash
filetools grep --input report.docx --pattern "Q1 revenue"
filetools grep --input report.docx --pattern "quarterly" --ignore-case --limit 5
```

| Option | Meaning |
|---|---|
| `--input <path>` | File to search. |
| `--pattern <s>` | Literal substring, matched per line of a block's text. |
| `--ignore-case` | Case-insensitive matching. |
| `--limit <n>` | Stop after N matches. 0 means no limit. |

### Output

```json
{
  "pattern": "paragraph",
  "returned": 2,
  "matches": [
    { "block_id": "el_8694f8af", "line": 1, "snippet": "First paragraph.",  "writable": true },
    { "block_id": "el_8eda9919", "line": 1, "snippet": "Second paragraph.", "writable": true }
  ]
}
```

| Field | Meaning |
|---|---|
| `block_id` | Stable id; feeds straight into `read --id` or a patch pointer. |
| `line` | 1-based line within the block's text. |
| `snippet` | The matching line, trimmed and truncated for display. |
| `writable` | Whether the format can be patched. `false` flags a match in a read-only format (locatable, not editable). |

## Library

```rust
use filetools_rs::grep;
use filetools_rs::model::GrepOptions;

let bytes = std::fs::read("report.xml")?;
let matches = grep(
    "report.xml",
    &bytes,
    "revenue",
    &GrepOptions { ignore_case: true, limit: Some(10) },
)?;
for m in &matches {
    println!("{} (line {}): {}", m.block_id, m.line, m.snippet);
}
# Ok::<(), anyhow::Error>(())
```

## Matching semantics

- `pattern` is a **literal substring**, not a regex.
- Matching is per line of each block's text content; a block that matches on
  several lines yields several matches.
- `grep` walks the same block tree `read` produces, so ids line up exactly with
  what `read`/`reconstruct` expect.
- **Spreadsheets (xlsx):** cell text is resolved before matching — shared
  strings (`t="s"`), inline strings (`t="inlineStr"`), formula string results
  (`t="str"`), booleans, and numeric/date values all participate. A cell hit is
  attributed to its row-range block (`sheet[n].rows[a-b]`), the id `read`
  accepts, so the loop funnels straight into hydration and edit. Sheet-name
  blocks (`sheet[n]`) also match.
- **Slides (pptx):** a slide's full paragraph text is searched, not just the
  truncated scan preview — including runs that carry attributes
  (`<a:t xml:space="preserve">`) and entity-encoded text. Hits are attributed to
  the slide block (`slide[n]`), which `read` hydrates to the slide's paragraphs.
- **Full-content reach beyond the preview.** For every format with a dedicated
  optimized scan — csv, markdown, html, mermaid, drawio, pptx, pdf, svg — `grep`
  searches the block's *full* hydrated content, not the truncated preview the
  scan manifest shows. So it finds a value in any CSV row (not just the first
  row of a chunk), section/paragraph text past the preview cutoff, deep diagram
  statements, and drawio cell labels (stored in the `value` attribute). Each hit
  is attributed to the scan-emitted block id (`rows[a-b]`, `section[n]`,
  `paragraph[n]`, `body[n]`/`subgraph:<name>`, `diagram:<name>`, …), which `read`
  hydrates to the full block.

## Typical loop

```bash
filetools grep --input deck.pptx --pattern "roadmap"      # find ids
filetools read --input deck.pptx --id <block_id>          # hydrate just those
# build a patch referencing the ids, then reconstruct
```
