# CLI usage

The `filetools` binary has five subcommands. JSON results print to stdout; a
one-line summary prints to stderr.

```
filetools <command> [options]

  extract      Extract a file to a semantic JSON envelope (+ sidecar id-map)
  scan         Print a lightweight manifest (no content hydrated) as JSON
  read         Hydrate specific blocks by id and print them as JSON
  grep         Search block text for a pattern and print matching ids as JSON
  reconstruct  Apply a patch to the original and write the reconstructed file
```

## extract

Project a file to an envelope JSON and write the sidecar id-map alongside it.

```bash
filetools extract --input report.xml --out report.ft.json
```

| Option | Meaning |
|---|---|
| `--input <path>` | File to extract. |
| `--out <path>` | Envelope output path. Defaults to `<input>.ft.json`. |
| `--readonly` | Strip ids, skip the sidecar — analysis-only, max token savings, not reconstructable. |

The sidecar (`<envelope>` + `.idmap.json`) is required for `reconstruct`; keep
it next to the envelope.

## scan

Print a lightweight manifest (structure and previews, no hydrated content) to
pick block ids before hydrating them.

```bash
filetools scan --input big.xlsx --offset 0 --limit 100
```

| Option | Meaning |
|---|---|
| `--input <path>` | File to scan. |
| `--offset <n>` | Skip the first N blocks. Default 0. |
| `--limit <n>` | Return at most N blocks. 0 means no limit. |

## read

Hydrate specific blocks by id (or all blocks if none given).

```bash
filetools read --input big.xlsx --id 'sheet[0].rows[0-99]'
filetools read --input report.docx --id paragraph[3] --id table[0]
```

| Option | Meaning |
|---|---|
| `--input <path>` | File to read. |
| `--id <id>` | Block id to hydrate. Repeatable. Accepts structural paths, `part:<name>` markers, and xlsx `sheet[n].rows[a-b]` ranges. With no ids, every block is returned. |
| `--offset <n>` / `--limit <n>` | Page the result. |

## grep

Locate blocks by text without hydrating the whole document — the discovery
counterpart to `read`. See [grep](grep.md) for details.

```bash
filetools grep --input report.docx --pattern "Q1 revenue" --ignore-case
```

| Option | Meaning |
|---|---|
| `--input <path>` | File to search. |
| `--pattern <s>` | Literal substring, matched per line of block text. |
| `--ignore-case` | Case-insensitive matching. |
| `--limit <n>` | Stop after N matches. 0 means no limit. |

Output: `{ pattern, returned, matches: [{ block_id, line, snippet, writable }] }`.
Each `block_id` feeds straight back into `read` or a patch.

## reconstruct

Apply a patch to the original and write the reconstructed file.

```bash
filetools reconstruct --envelope report.ft.json --patch patch.json \
                      --out report_v2.xml
```

| Option | Meaning |
|---|---|
| `--envelope <path>` | The envelope produced by `extract` (its sidecar must sit alongside). |
| `--patch <path>` | Patch JSON: `{ "patch": [ ... ] }`. See [patch format](patch-format.md). |
| `--out <path>` | Output path for the reconstructed file. |
| `--original <path>` | Source file. Defaults to the path recorded in the envelope. |

If the original was rewritten out-of-band since extract, reconstruct recovers
autonomously and prints a `drift recovery:` note to stderr (or refuses if a
target can no longer be located). See [drift recovery](drift-recovery.md).

## Typical loop

```bash
filetools extract --input deck.pptx                    # envelope + sidecar
filetools grep    --input deck.pptx --pattern "roadmap" # find the block ids
filetools read    --input deck.pptx --id 'part:ppt/slides/slide3.xml'
# build patch.json referencing the ids you found, then:
filetools reconstruct --envelope deck.pptx.ft.json --patch patch.json \
                      --out deck_v2.pptx
```
