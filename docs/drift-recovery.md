# Drift recovery

"Drift" is when the original file changes between `extract` and `reconstruct` —
most commonly because another tool rewrote it (e.g. openpyxl re-emitting an
xlsx, or a formatter reflowing XML). The id-map was bound to the *old* bytes, so
its byte spans no longer point where they did.

filetools recovers from this **autonomously** instead of failing.

## What happens on reconstruct

1. **Match check.** If `sha256(original)` equals the hash the envelope was
   extracted from, nothing drifted — apply directly.
2. **Drift detected.** Otherwise, re-extract the *current* bytes into a fresh
   structure + id-map.
3. **Re-target.** For each op, compute a **parent-anchored semantic
   fingerprint** of its target from the envelope it was authored against, then
   find the node with the same fingerprint in the fresh structure and rewrite
   the op's id to the new one.
4. **Apply atomically** against the fresh id-map.

A one-line note prints to stderr on recovery, e.g.:

```
drift recovery: original `book.xlsx` was rewritten since extract; re-targeted 1 of 1 ops by content fingerprint
```

## Why a semantic fingerprint (not a byte hash)

The id-map's per-node `hash` is a hash of the element's *raw bytes*. An external
rewriter changes those bytes (whitespace, attribute order, added attributes,
re-emitted markup) even when the **value** is unchanged — so a byte hash would
match nothing after exactly the rewrite we want to survive.

The fingerprint instead keys on what the rewrite preserves:

- **signature** = the node's tag + its normalized descendant text (whitespace
  collapsed, trimmed). Container nodes are identified by the text they enclose.
- **parent signature** = the same, for the node's parent.

Parent-anchoring disambiguates repeated content (identical cells/paragraphs) so
a re-target lands on the node the patch *meant*, not merely one that looks the
same.

This survives whitespace reflow, attribute reordering, entity reencoding, and
wholesale serializer rewrites.

## When recovery refuses (fail-loud)

Recovery is all-or-nothing. The entire patch is refused, and **nothing is
written**, if any target after the rewrite is:

- **gone** — no node in the fresh structure has its fingerprint
  (`target ... no longer exists after the original was rewritten`), or
- **ambiguous** — more than one node shares its fingerprint
  (`target ... is ambiguous after re-extract`).

Refusing is the only safe response when the content an edit meant to change can
no longer be located unambiguously — better than editing the wrong node.

## Scope

Recovery applies to writable, id-map-bearing formats (XML, OOXML, drawio, CSV,
HTML). Read-only formats have nothing to reconstruct, so drift is moot there.

## Authoring patches that recover well

- Prefer editing nodes with distinctive text; identical sibling values rely on
  parent-anchoring and can still go ambiguous if the parent text is identical
  too.
- An empty patch on a drifted file is a safe no-op (it recovers to "nothing to
  do" and returns the current bytes).
