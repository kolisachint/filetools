# Patch format

A patch is an RFC-6902-style op list adapted to id-based pointers. The agent
returns it; `reconstruct` (CLI) / `write` (library) applies it.

```json
{ "patch": [
  { "op": "test",    "path": "/structure/el_8694f8af", "hash": "sha256:…" },
  { "op": "replace", "path": "/structure/el_8694f8af/text", "value": "Revenue grew 18%." },
  { "op": "add",     "after": "el_8694f8af",
    "value": { "tag": "p", "attrs": [{"name":"id","value":"p2"}], "text": "See appendix A." } },
  { "op": "remove",  "path": "/structure/el_old" }
] }
```

## Pointers

Pointers are id-based, resolved through the id-map (not array indices, so they
survive edits):

- `/structure/<id>/text` — element text content
- `/structure/<id>/attrs/<name>` — an attribute value
- `/structure/<id>` — the element itself (for `remove`)

## Ops

| Op | Fields | Effect |
|---|---|---|
| `test` | `path`, `hash` | Optimistic concurrency guard: the target's current content hash must equal `hash`, or the whole patch aborts. |
| `replace` | `path`, `value` | Replace element text or an attribute value. |
| `add` | `after` \| `before`, `value` | Insert a new element anchored to a stable neighbour id. |
| `remove` | `path` | Delete an element and all its bytes. |

`value` for `add` is a `NewElement`: `{ tag, attrs: [{name, value}], text }`.

## Atomicity

Any failed op or stale `test` guard aborts the **whole** patch; the original is
left untouched. Multi-part containers (e.g. several pptx slides) are all-or-
nothing too: if any part fails, the container is not rebuilt.

## Concurrency and staleness

Two mechanisms keep an edit from landing on content that changed underneath it:

1. **Explicit `test` op** — carries the expected content hash. Validated
   against the id-map's per-node `hash` before any byte is written.
2. **Autonomous drift recovery** — if the *file* was rewritten out-of-band
   since extract, reconstruct re-targets ops by semantic fingerprint or refuses.
   See [drift recovery](drift-recovery.md).

## Envelope (what the agent sees)

`id`s are stable, content-addressed, and used to address edits.

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

## Sidecar id-map (never shown to the agent)

Maps each id to byte spans + a guard hash, bound to the original by hash:

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

The sidecar is required for `reconstruct`. Keep it next to the envelope (its
name is in `idmap_ref`).
