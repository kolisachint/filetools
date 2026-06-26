//! Core wire types: the extract envelope, semantic nodes, and fidelity levels.
//!
//! The envelope is what the LLM sees. The original file stays the source of
//! truth; the envelope is a *projection* of it, addressable by stable ids.

use serde::{Deserialize, Serialize};

/// How faithfully a handler can reconstruct a file after edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// Byte-identical reproduction of untouched content guaranteed
    /// (verify-on-extract enforced). XML, drawio, OOXML.
    Lossless,
    /// Untouched bytes preserved; edits are surgical text-level only,
    /// rejected if they don't fit the existing layout. PDF.
    InPlaceText,
    /// Best-effort extraction only; no write-back. Unknown binary.
    ReadOnly,
}

/// Identifies the original file and pins its content for integrity checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub path: String,
    /// Logical format, e.g. "xml", "drawio".
    pub r#type: String,
    /// `sha256:<hex>` of the original bytes. Reconstruct verifies this.
    pub hash: String,
}

/// The extract output handed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub version: String,
    pub source: Source,
    pub fidelity: Fidelity,
    pub writable: bool,
    /// Filename of the sidecar id-map needed for reconstruct. Absent for
    /// read-only output (which carries no addressable ids worth patching).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idmap_ref: Option<String>,
    pub structure: Vec<DocNode>,
}

/// A semantic node. For the generic XML core every node is an `Element`;
/// higher-level handlers (drawio, OOXML) may add richer variants later, but
/// they all resolve back to byte ranges in the original via the id-map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocNode {
    pub id: String,
    /// Element tag name as it appears in the source (e.g. `mxCell`, `w:p`).
    pub tag: String,
    /// Attributes in document order. Shown for context; editable where the
    /// handler tracks the attribute's byte range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<Attr>,
    /// Direct text content, present only when the element has a single
    /// contiguous text run and no child elements (so a text-replace is
    /// unambiguous and lossless).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DocNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attr {
    pub name: String,
    pub value: String,
}

// ── Manifest-first API types ──────────────────────────────────────────────

/// Lightweight document manifest returned by `scan()`.
/// Contains metadata for block selection without hydrating full content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub file_type: FileType,
    pub block_count: usize,
    pub total_tokens: usize,
    pub blocks: Vec<BlockManifest>,
}

/// Per-block metadata for agent selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockManifest {
    /// Structural path ID: `section[0]`, `section[0].paragraph[1]`, etc.
    pub id: String,
    pub block_type: BlockType,
    /// First ~100 chars for disambiguation.
    pub preview: String,
    /// Content hash for staleness detection.
    pub content_hash: String,
    /// Parent block ID, derived from path.
    pub parent_id: Option<String>,
    /// Rough token estimate (preview.len() / 4).
    pub token_estimate: usize,
    pub section_name: String,
    pub section_number: usize,
}

/// Logical file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Markdown,
    Ooxml,
    Xml,
    Drawio,
    Pdf,
    Csv,
    Html,
    Image,
    Archive,
    Mermaid,
    Unknown,
}

/// A single content match returned by `grep()`.
///
/// `block_id` is a structural id that resolves directly via `read()`/`edit()`,
/// so a caller can feed matches straight back into the funnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub block_id: String,
    /// 1-based line number within the block's text content.
    pub line: usize,
    /// The matching line, truncated for display.
    pub snippet: String,
}

/// Options controlling `grep()`.
#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    /// Case-insensitive matching.
    pub ignore_case: bool,
    /// Stop after this many matches. `None` means unbounded.
    pub limit: Option<usize>,
}

/// Block type for manifest entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    Heading,
    Paragraph,
    Table,
    List,
    Code,
    Image,
    Cell,
    Section,
    Other,
}
