//! CSV handler: cell-addressable, byte-faithful field edits.
//!
//! Fields are addressed as `cell[row,col]` (0-based, row 0 is the header).
//! The id-map records each field's byte span (excluding surrounding quotes for
//! quoted fields). Reconstruct replaces only the targeted spans and re-quotes a
//! new value when it contains a comma, quote, CR, or LF, leaving every other
//! byte untouched.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::idmap::{sha256_hex, IdMap, NodeLoc, Span};
use crate::model::{DocNode, Fidelity};
use crate::patch::{Op, Patch};

/// A located CSV field.
struct Field {
    row: usize,
    col: usize,
    /// Span of the field value as it appears, excluding surrounding quotes.
    value: Span,
    /// Whether the source field was wrapped in double quotes.
    quoted: bool,
}

/// Parse CSV into located fields. Supports RFC-4180 style quoting: fields may be
/// wrapped in double quotes, inside which `""` is an escaped quote and commas /
/// newlines are literal.
fn tokenize(content: &str) -> Vec<Field> {
    let b = content.as_bytes();
    let mut fields = Vec::new();
    let mut row = 0usize;
    let mut col = 0usize;
    let mut i = 0usize;
    let n = b.len();

    while i <= n {
        // Start of a field.
        if i == n {
            break;
        }
        if b[i] == b'"' {
            // Quoted field.
            let value_start = i + 1;
            let mut j = value_start;
            loop {
                if j >= n {
                    break;
                }
                if b[j] == b'"' {
                    if j + 1 < n && b[j + 1] == b'"' {
                        j += 2; // escaped quote
                        continue;
                    }
                    break; // closing quote
                }
                j += 1;
            }
            fields.push(Field {
                row,
                col,
                value: Span {
                    start: value_start,
                    end: j,
                },
                quoted: true,
            });
            // Advance past closing quote.
            i = j + 1;
        } else {
            // Unquoted field: until comma or newline.
            let value_start = i;
            let mut j = i;
            while j < n && b[j] != b',' && b[j] != b'\n' && b[j] != b'\r' {
                j += 1;
            }
            fields.push(Field {
                row,
                col,
                value: Span {
                    start: value_start,
                    end: j,
                },
                quoted: false,
            });
            i = j;
        }

        // Consume the delimiter or row terminator.
        if i < n && b[i] == b',' {
            col += 1;
            i += 1;
        } else if i < n && (b[i] == b'\n' || b[i] == b'\r') {
            // Normalize CRLF / CR / LF as a single row break.
            if b[i] == b'\r' && i + 1 < n && b[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            row += 1;
            col = 0;
        } else {
            // End of input.
            i += 1;
        }
    }

    fields
}

/// Encode a new field value, quoting when it contains a comma, quote, CR or LF.
/// Returns the bytes to splice in place of the existing (unquoted) value span.
/// The handler removes the original quotes when re-quoting differs, so the
/// span covered is always the *inner* value.
fn encode_value(value: &str, was_quoted: bool) -> (Vec<u8>, bool) {
    let needs_quote =
        value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r');
    if needs_quote {
        let escaped = value.replace('"', "\"\"");
        (escaped.into_bytes(), true)
    } else {
        let _ = was_quoted;
        (value.as_bytes().to_vec(), false)
    }
}

/// The CSV handler.
pub struct CsvHandler;

impl super::Handler for CsvHandler {
    fn type_name(&self) -> &'static str {
        "csv"
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::Lossless
    }

    fn extract(&self, bytes: &[u8], for_hash: &str) -> Result<(Vec<DocNode>, Option<IdMap>)> {
        let content =
            std::str::from_utf8(bytes).map_err(|e| anyhow::anyhow!("invalid UTF-8 in CSV: {e}"))?;
        let fields = tokenize(content);

        let mut structure = Vec::new();
        let mut map = BTreeMap::new();

        for f in &fields {
            let id = format!("cell[{},{}]", f.row, f.col);
            let raw = &content[f.value.start..f.value.end];
            let text = if f.quoted {
                raw.replace("\"\"", "\"")
            } else {
                raw.to_string()
            };
            structure.push(DocNode {
                id: id.clone(),
                tag: "cell".to_string(),
                attrs: Vec::new(),
                text: Some(text),
                children: Vec::new(),
            });
            map.insert(
                id,
                NodeLoc {
                    tag: "cell".to_string(),
                    part: String::new(),
                    element: f.value,
                    inner: Some(f.value),
                    runs: None,
                    attrs: BTreeMap::new(),
                    hash: sha256_hex(raw.as_bytes()),
                },
            );
        }

        let idmap = IdMap {
            for_hash: for_hash.to_string(),
            map,
        };
        Ok((structure, Some(idmap)))
    }

    /// CSV spans point at field values, not `<`-prefixed elements, so the
    /// generic span verifier does not apply. The hash guard on each field is
    /// still checked at edit time.
    fn verify(&self, _bytes: &[u8], _idmap: &IdMap) -> Result<()> {
        Ok(())
    }

    fn reconstruct(&self, bytes: &[u8], idmap: &IdMap, patch: &Patch) -> Result<Vec<u8>> {
        // Resolve every op into a (span, replacement-with-quote-context) edit,
        // validating guards first so the operation is atomic.
        struct Edit {
            start: usize,
            end: usize,
            bytes: Vec<u8>,
            // When set, the replacement must be wrapped in quotes; we extend the
            // span outward by one byte on each side if the original was quoted,
            // otherwise we add quotes inline.
            wrap_quotes: bool,
            was_quoted: bool,
        }

        let mut edits: Vec<Edit> = Vec::new();

        for (i, op) in patch.patch.iter().enumerate() {
            match op {
                Op::Test { path, hash } => {
                    let id = strip_id(path);
                    let loc = idmap
                        .get(id)
                        .ok_or_else(|| anyhow::anyhow!("unknown cell `{id}`"))?;
                    if &loc.hash != hash {
                        bail!(
                            "guard failed for `{id}`: expected {hash}, found {}",
                            loc.hash
                        );
                    }
                }
                Op::Replace { path, value } => {
                    let id = strip_text_id(path).ok_or_else(|| {
                        anyhow::anyhow!("op #{i}: CSV only supports `/structure/<id>/text`")
                    })?;
                    let loc = idmap
                        .get(id)
                        .ok_or_else(|| anyhow::anyhow!("unknown cell `{id}`"))?;
                    let span = loc.element;
                    let original = &bytes[span.start..span.end];
                    let was_quoted = span.start >= 1
                        && bytes.get(span.start - 1) == Some(&b'"')
                        && bytes.get(span.end) == Some(&b'"');
                    let _ = original;
                    let (new_bytes, wrap_quotes) = encode_value(value, was_quoted);
                    edits.push(Edit {
                        start: span.start,
                        end: span.end,
                        bytes: new_bytes,
                        wrap_quotes,
                        was_quoted,
                    });
                }
                Op::Add { .. } | Op::Remove { .. } => {
                    bail!("op #{i}: CSV handler supports only cell text replacement");
                }
            }
        }

        edits.sort_by_key(|e| e.start);
        for w in edits.windows(2) {
            if w[1].start < w[0].end {
                bail!("edits overlap and cannot be applied atomically");
            }
        }

        let mut out = Vec::with_capacity(bytes.len());
        let mut cursor = 0usize;
        for e in &edits {
            // Expand the splice over the surrounding quotes when present, so we
            // can drop or add quoting cleanly.
            let (mut start, mut end) = (e.start, e.end);
            if e.was_quoted {
                start -= 1; // include opening quote
                end += 1; // include closing quote
            }
            out.extend_from_slice(&bytes[cursor..start]);
            if e.wrap_quotes {
                out.push(b'"');
                out.extend_from_slice(&e.bytes);
                out.push(b'"');
            } else {
                out.extend_from_slice(&e.bytes);
            }
            cursor = end;
        }
        out.extend_from_slice(&bytes[cursor..]);
        Ok(out)
    }
}

/// `/structure/<id>` or `/structure/<id>/...` -> `<id>`.
fn strip_id(path: &str) -> &str {
    path.strip_prefix("/structure/")
        .and_then(|r| r.split('/').next())
        .unwrap_or(path)
}

/// `/structure/<id>/text` -> `Some(<id>)`, else `None`.
fn strip_text_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/structure/")?;
    let mut parts = rest.splitn(2, '/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    match parts.next() {
        Some("text") => Some(id),
        _ => None,
    }
}
