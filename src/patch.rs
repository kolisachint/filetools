//! The patch wire format and applier.
//!
//! RFC 6902 op vocabulary, adapted per the design decisions:
//!   * Pointers are *id-based*: `/structure/<id>/text`, resolved through the
//!     sidecar id-map at apply time (not array indices).
//!   * `add` is anchored to a stable neighbour via `after`/`before`, or
//!     appended to a container with no anchor.
//!   * `test` carries a content `hash` (not a value) for optimistic guards.
//!   * Application is atomic: any failed op/guard aborts; original untouched.

use serde::{Deserialize, Serialize};

use crate::idmap::{IdMap, Span};

/// A new element to insert. Text-only content (the v1 inline-text decision).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewElement {
    pub tag: String,
    #[serde(default)]
    pub attrs: Vec<crate::model::Attr>,
    #[serde(default)]
    pub text: Option<String>,
}

/// One patch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Guard: assert the target node's content hash before mutating.
    Test { path: String, hash: String },
    /// Replace an element's text (`/structure/<id>/text`) or an attribute
    /// value (`/structure/<id>/attrs/<name>`).
    Replace { path: String, value: String },
    /// Insert a new element next to an anchor, or append into a container.
    Add {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        value: NewElement,
    },
    /// Delete an element (`/structure/<id>`) and all its bytes.
    Remove { path: String },
}

impl Op {
    /// The node id this op references — its target's id, or an `add`'s anchor.
    /// Used to route ops to the right container part.
    pub fn target_id(&self) -> Option<&str> {
        match self {
            Op::Test { path, .. } | Op::Replace { path, .. } | Op::Remove { path } => {
                pointer_id(path)
            }
            Op::Add { after, before, .. } => after.as_deref().or(before.as_deref()),
        }
    }
}

/// Extract the `<id>` from a `/structure/<id>...` pointer.
fn pointer_id(path: &str) -> Option<&str> {
    path.strip_prefix("/structure/")?
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub patch: Vec<Op>,
}

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("unknown node id `{0}`")]
    UnknownId(String),
    #[error("guard failed for `{id}`: expected {expected}, found {found}")]
    GuardFailed {
        id: String,
        expected: String,
        found: String,
    },
    #[error("op #{0} targets `{1}` which is not text-replaceable (mixed/empty content)")]
    NotTextReplaceable(usize, String),
    #[error("op #{op}: attribute `{attr}` not found / not editable on `{id}`")]
    UnknownAttr { op: usize, id: String, attr: String },
    #[error("op #{0}: malformed path `{1}`")]
    BadPath(usize, String),
    #[error("op #{0}: `add` needs exactly one of `after`/`before`")]
    BadAnchor(usize),
    #[error("edits overlap and cannot be applied atomically")]
    OverlappingEdits,
}

/// A resolved byte-range edit: replace `range` with `bytes`.
struct Edit {
    range: Span,
    bytes: Vec<u8>,
}

/// Parse a `/structure/<id>(/text|/attrs/<name>)?` pointer.
enum Target<'a> {
    Element(&'a str),
    Text(&'a str),
    Attr(&'a str, &'a str),
}

fn parse_pointer(p: &str) -> Option<Target<'_>> {
    let rest = p.strip_prefix("/structure/")?;
    let mut parts = rest.splitn(3, '/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    match parts.next() {
        None => Some(Target::Element(id)),
        Some("text") if parts.next().is_none() => Some(Target::Text(id)),
        Some("attrs") => parts.next().map(|name| Target::Attr(id, name)),
        Some(_) => None,
    }
}

/// Serialize a new element to bytes: `<tag a="b">text</tag>` or `<tag a="b"/>`.
fn serialize(el: &NewElement) -> Vec<u8> {
    let mut s = String::new();
    s.push('<');
    s.push_str(&el.tag);
    for a in &el.attrs {
        s.push(' ');
        s.push_str(&a.name);
        s.push_str("=\"");
        s.push_str(&xml_escape_attr(&a.value));
        s.push('"');
    }
    match &el.text {
        Some(t) => {
            s.push('>');
            s.push_str(&xml_escape_text(t));
            s.push_str("</");
            s.push_str(&el.tag);
            s.push('>');
        }
        None => s.push_str("/>"),
    }
    s.into_bytes()
}

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_escape_attr(s: &str) -> String {
    xml_escape_text(s).replace('"', "&quot;")
}

/// Apply a patch to the original bytes, returning the new bytes.
///
/// Atomic: validates every guard and resolves every edit before writing a
/// single byte. On any error the caller's original is left untouched.
pub fn apply(original: &[u8], idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>, PatchError> {
    let mut edits: Vec<Edit> = Vec::new();

    for (i, op) in patch.patch.iter().enumerate() {
        match op {
            Op::Test { path, hash } => {
                let id = match parse_pointer(path) {
                    Some(Target::Element(id))
                    | Some(Target::Text(id))
                    | Some(Target::Attr(id, _)) => id,
                    None => return Err(PatchError::BadPath(i, path.clone())),
                };
                let loc = idmap
                    .get(id)
                    .ok_or_else(|| PatchError::UnknownId(id.to_string()))?;
                if &loc.hash != hash {
                    return Err(PatchError::GuardFailed {
                        id: id.to_string(),
                        expected: hash.clone(),
                        found: loc.hash.clone(),
                    });
                }
            }
            Op::Replace { path, value } => {
                match parse_pointer(path).ok_or_else(|| PatchError::BadPath(i, path.clone()))? {
                    Target::Text(id) => {
                        let loc = idmap
                            .get(id)
                            .ok_or_else(|| PatchError::UnknownId(id.to_string()))?;
                        let inner = loc
                            .inner
                            .ok_or_else(|| PatchError::NotTextReplaceable(i, id.to_string()))?;
                        edits.push(Edit {
                            range: inner,
                            bytes: xml_escape_text(value).into_bytes(),
                        });
                    }
                    Target::Attr(id, name) => {
                        let loc = idmap
                            .get(id)
                            .ok_or_else(|| PatchError::UnknownId(id.to_string()))?;
                        let span = loc.attrs.get(name).copied().ok_or_else(|| {
                            PatchError::UnknownAttr {
                                op: i,
                                id: id.to_string(),
                                attr: name.to_string(),
                            }
                        })?;
                        edits.push(Edit {
                            range: span,
                            bytes: xml_escape_attr(value).into_bytes(),
                        });
                    }
                    Target::Element(_) => return Err(PatchError::BadPath(i, path.clone())),
                }
            }
            Op::Remove { path } => {
                let id = match parse_pointer(path)
                    .ok_or_else(|| PatchError::BadPath(i, path.clone()))?
                {
                    Target::Element(id) => id,
                    _ => return Err(PatchError::BadPath(i, path.clone())),
                };
                let loc = idmap
                    .get(id)
                    .ok_or_else(|| PatchError::UnknownId(id.to_string()))?;
                edits.push(Edit {
                    range: loc.element,
                    bytes: Vec::new(),
                });
            }
            Op::Add {
                after,
                before,
                value,
            } => {
                let (anchor, at_end) = match (after, before) {
                    (Some(a), None) => (a, true),
                    (None, Some(b)) => (b, false),
                    _ => return Err(PatchError::BadAnchor(i)),
                };
                let loc = idmap
                    .get(anchor)
                    .ok_or_else(|| PatchError::UnknownId(anchor.to_string()))?;
                let at = if at_end {
                    loc.element.end
                } else {
                    loc.element.start
                };
                edits.push(Edit {
                    range: Span { start: at, end: at },
                    bytes: serialize(value),
                });
            }
        }
    }

    splice(original, edits)
}

/// Apply non-overlapping byte edits. Inserts (zero-width ranges) are allowed
/// to share a boundary with each other but not to fall inside a replaced span.
fn splice(original: &[u8], mut edits: Vec<Edit>) -> Result<Vec<u8>, PatchError> {
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    // Reject true overlaps (one edit's interior crossing another's).
    for w in edits.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.range.start < a.range.end {
            return Err(PatchError::OverlappingEdits);
        }
    }
    let mut out = Vec::with_capacity(original.len());
    let mut cursor = 0usize;
    for e in &edits {
        out.extend_from_slice(&original[cursor..e.range.start]);
        out.extend_from_slice(&e.bytes);
        cursor = e.range.end;
    }
    out.extend_from_slice(&original[cursor..]);
    Ok(out)
}
