//! filetools: a reversible, token-efficient file serialization format for LLMs.
//!
//! Extract a file to compact semantic JSON (the *envelope*) plus a sidecar
//! id-map. An LLM edits nodes and returns an RFC-6902-style patch. Reconstruct
//! applies the patch by splicing edits into the original's byte spans, leaving
//! all untouched content byte-for-byte intact.
//!
//! Design guarantees, by handler fidelity:
//!   * `Lossless`   (xml, drawio): untouched bytes reproduced exactly;
//!     verify-on-extract enforces span correctness before output is trusted.
//!   * `InPlaceText` (pdf, planned): surgical text edits only.
//!   * `ReadOnly`   (unknown binary): extract-only, `writable: false`.

pub mod cache;
pub mod handlers;
pub mod idmap;
pub mod model;
pub mod patch;

use std::collections::HashMap;
use std::sync::Mutex;

use cache::LruCache;

use anyhow::{bail, Context, Result};

use idmap::{sha256_hex, IdMap};
use model::{
    Attr, BlockManifest, BlockType, DocNode, Envelope, Fidelity, FileType, GrepMatch, GrepOptions,
    ScanResult, Source,
};
use patch::Patch;

/// Maximum number of files retained in each in-memory cache.
const CACHE_CAPACITY: usize = 32;

/// In-memory bounded cache for scan results, keyed by content hash.
static SCAN_CACHE: Mutex<Option<LruCache<ScanResult>>> = Mutex::new(None);

/// In-memory bounded cache for extract results (envelope + idmap).
static EXTRACT_CACHE: Mutex<Option<LruCache<ExtractOutput>>> = Mutex::new(None);

/// Get or initialize the scan cache.
fn scan_cache() -> std::sync::MutexGuard<'static, Option<LruCache<ScanResult>>> {
    SCAN_CACHE.lock().unwrap()
}

/// Get or initialize the extract cache.
fn extract_cache() -> std::sync::MutexGuard<'static, Option<LruCache<ExtractOutput>>> {
    EXTRACT_CACHE.lock().unwrap()
}

/// Outcome of extracting a file.
#[derive(Clone)]
pub struct ExtractOutput {
    pub envelope: Envelope,
    /// Present for writable (id-map-bearing) formats.
    pub idmap: Option<IdMap>,
}

/// Extract `bytes` (originating from `path`) into an envelope + sidecar.
///
/// For `Lossless` handlers this runs verify-on-extract: it confirms every
/// recorded span actually points at its element and that hashes recompute, so
/// a downstream reconstruct can be trusted to be byte-faithful.
pub fn extract(path: &str, bytes: &[u8]) -> Result<ExtractOutput> {
    let hash = sha256_hex(bytes);

    // Check extract cache first
    {
        let mut cache = extract_cache();
        if let Some(cached) = cache.as_mut().and_then(|c| c.get(&hash)) {
            return Ok(cached.clone());
        }
    }

    let handler = handlers::for_path(path, bytes);
    let fidelity = handler.fidelity();
    let type_name = handler.type_name();

    let (structure, idmap) = handler.extract(bytes, &hash)?;

    if fidelity == Fidelity::Lossless {
        let map = idmap
            .as_ref()
            .context("lossless handler produced no id-map")?;
        if map.for_hash != hash {
            bail!("id-map is bound to a different original");
        }
        handler
            .verify(bytes, map)
            .context("verify-on-extract failed: handler is not byte-faithful for this input")?;
    }

    let idmap_ref = idmap
        .as_ref()
        .map(|_| format!("{}.idmap.json", file_name(path)));
    let envelope = Envelope {
        version: "1.0".to_string(),
        source: Source {
            path: path.to_string(),
            r#type: type_name.to_string(),
            hash: hash.clone(),
        },
        fidelity,
        writable: idmap.is_some(),
        idmap_ref,
        structure,
    };

    let result = ExtractOutput { envelope, idmap };

    // Store in cache
    {
        let mut cache = extract_cache();
        cache
            .get_or_insert_with(|| LruCache::new(CACHE_CAPACITY))
            .insert(hash, result.clone());
    }

    Ok(result)
}

/// Scan a file and return a lightweight manifest without full content hydration.
///
/// Results are cached in memory. Subsequent calls for the same path return
/// the cached result if available.
pub fn scan(path: &str, bytes: &[u8]) -> Result<ScanResult> {
    // Key the cache by content hash so a changed file is never served stale.
    let cache_key = sha256_hex(bytes);

    // Check cache first
    {
        let mut cache = scan_cache();
        if let Some(cached) = cache.as_mut().and_then(|c| c.get(&cache_key)) {
            return Ok(cached.clone());
        }
    }

    let result = scan_uncached(path, bytes)?;

    // Store in cache
    {
        let mut cache = scan_cache();
        cache
            .get_or_insert_with(|| LruCache::new(CACHE_CAPACITY))
            .insert(cache_key, result.clone());
    }

    Ok(result)
}

/// Compute a fresh scan manifest, dispatching to the optimized handler for the
/// file's extension and falling back to generic extraction otherwise.
fn scan_uncached(path: &str, bytes: &[u8]) -> Result<ScanResult> {
    if path.ends_with(".xlsx") {
        return scan_xlsx_manifest(path, bytes);
    } else if path.ends_with(".pptx") {
        return scan_pptx_manifest(path, bytes);
    } else if path.ends_with(".pdf") {
        return scan_pdf_manifest(path, bytes);
    } else if path.ends_with(".svg") {
        return scan_svg_manifest(path, bytes);
    } else if path.ends_with(".drawio") || path.ends_with(".dio") {
        return scan_drawio_manifest(path, bytes);
    } else if path.ends_with(".md") || path.ends_with(".markdown") {
        return scan_markdown_manifest(path, bytes);
    } else if path.ends_with(".mmd") || path.ends_with(".mermaid") {
        return scan_mermaid_manifest(path, bytes);
    } else if path.ends_with(".csv") {
        return scan_csv_manifest(path, bytes);
    } else if path.ends_with(".zip") {
        return scan_zip_manifest(path, bytes);
    } else if path.ends_with(".html") || path.ends_with(".htm") {
        return scan_html_manifest(path, bytes);
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") || path.ends_with(".png") {
        return scan_binary_manifest(path, bytes);
    }

    // Fallback to generic extraction
    let extract_out = extract(path, bytes)?;
    let file_type = map_file_type(&extract_out.envelope.source.r#type);
    let mut blocks = Vec::new();
    let mut section_counter = 0usize;

    build_manifest(
        &extract_out.envelope.structure,
        file_type,
        &mut blocks,
        &mut section_counter,
        None,
        &mut None,
    );

    let total_tokens = blocks.iter().map(|b| b.token_estimate).sum();
    Ok(ScanResult {
        file_type,
        block_count: blocks.len(),
        total_tokens,
        blocks,
    })
}

/// Build block manifests from the document tree.
///
/// When `index` is `Some`, each block's id is also mapped to a clone of its
/// source node, so `read()` can resolve the exact ids `scan()` produced without
/// a second, divergent traversal.
fn build_manifest(
    nodes: &[DocNode],
    file_type: FileType,
    blocks: &mut Vec<BlockManifest>,
    section_counter: &mut usize,
    parent_id: Option<String>,
    index: &mut Option<HashMap<String, DocNode>>,
) {
    for (idx, node) in nodes.iter().enumerate() {
        // For OOXML tables, decompose into rows and cells
        if file_type == FileType::Ooxml && is_table_node(node) {
            decompose_table(node, file_type, blocks, section_counter, &parent_id, index);
            continue;
        }

        let block_type = map_block_type(node, file_type);
        let preview = extract_preview(node, block_type);
        let content_hash = node.id.clone();
        let token_estimate = estimate_tokens(&preview);

        // Generate structural path ID
        let id = generate_structural_id(node, idx, file_type, section_counter, blocks, &parent_id);

        let section_name = if block_type == BlockType::Heading || block_type == BlockType::Section {
            node.text.clone().unwrap_or_default()
        } else {
            String::new()
        };

        let section_number = *section_counter;
        if block_type == BlockType::Heading || block_type == BlockType::Section {
            *section_counter += 1;
        }

        if let Some(map) = index.as_mut() {
            map.insert(id.clone(), node.clone());
        }

        blocks.push(BlockManifest {
            id,
            block_type,
            preview,
            content_hash,
            parent_id: parent_id.clone(),
            token_estimate,
            section_name,
            section_number,
        });

        // Recurse into children with current block's ID as parent
        if !node.children.is_empty() {
            let current_id = blocks.last().map(|b| b.id.clone());
            build_manifest(
                &node.children,
                file_type,
                blocks,
                section_counter,
                current_id,
                index,
            );
        }
    }
}

/// Check if a node is an OOXML table.
fn is_table_node(node: &DocNode) -> bool {
    matches!(node.tag.as_str(), "w:tbl" | "a:tbl")
}

/// Scan XLSX file using hierarchical blocks.
/// Returns sheet-level and row-range blocks instead of cell-per-block.
fn scan_xlsx_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let xlsx_result = handlers::xlsx::scan_xlsx(bytes)?;

    let mut blocks = Vec::new();

    // Convert XLSX structure to BlockManifest entries
    for node in &xlsx_result.structure {
        let block_type = match node.tag.as_str() {
            "_sheet" => BlockType::Section,
            "_row_range" => BlockType::List,
            _ => BlockType::Other,
        };

        let preview = node.text.clone().unwrap_or_default();
        let token_estimate = estimate_tokens(&preview);

        blocks.push(BlockManifest {
            id: node.id.clone(),
            block_type,
            preview,
            content_hash: String::new(), // Hierarchical blocks don't have content hashes
            parent_id: None,
            token_estimate,
            section_name: String::new(),
            section_number: blocks.len(),
        });
    }

    let total_tokens = blocks.iter().map(|b| b.token_estimate).sum();

    Ok(ScanResult {
        file_type: FileType::Ooxml,
        block_count: blocks.len(),
        total_tokens,
        blocks,
    })
}

/// Scan PPTX with slide-level blocks.
fn scan_pptx_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_pptx(bytes)?;
    Ok(manifest_from_optimized(result, &["_slide"]))
}

/// Scan PDF with page-level blocks.
fn scan_pdf_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_pdf(bytes)?;
    Ok(manifest_from_optimized(result, &["_page"]))
}

/// Scan SVG with element-level blocks.
fn scan_svg_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_svg(bytes)?;
    Ok(manifest_from_optimized(result, &["_svg_group"]))
}

/// Scan Drawio with diagram-level blocks.
fn scan_drawio_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_drawio(bytes)?;
    Ok(manifest_from_optimized(result, &["_diagram_block"]))
}

/// Scan Markdown with section-level blocks.
fn scan_markdown_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_markdown(bytes)?;
    Ok(manifest_from_optimized(result, &["_section"]))
}

/// Scan binary format (JPG, PNG) with metadata blocks.
fn scan_binary_manifest(path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_binary(path, bytes)?;
    Ok(manifest_from_optimized(result, &[]))
}

/// Convert an optimized scan result into a manifest `ScanResult`.
/// Section-like blocks are tagged `BlockType::Section`, everything else
/// `BlockType::Other`.
fn manifest_from_optimized(
    result: handlers::optimized::OptimizedScanResult,
    section_tags: &[&str],
) -> ScanResult {
    let mut blocks = Vec::new();
    for node in &result.structure {
        let block_type = if section_tags.contains(&node.tag.as_str()) {
            BlockType::Section
        } else {
            BlockType::Other
        };
        let preview = node.text.clone().unwrap_or_default();
        let token_estimate = estimate_tokens(&preview);
        blocks.push(BlockManifest {
            id: node.id.clone(),
            block_type,
            preview,
            content_hash: String::new(),
            parent_id: None,
            token_estimate,
            section_name: String::new(),
            section_number: blocks.len(),
        });
    }
    let total_tokens = blocks.iter().map(|b| b.token_estimate).sum();
    ScanResult {
        file_type: result.file_type,
        block_count: blocks.len(),
        total_tokens,
        blocks,
    }
}

/// Scan a Mermaid (.mmd) diagram into diagram/subgraph blocks.
fn scan_mermaid_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_mermaid(bytes)?;
    Ok(manifest_from_optimized(
        result,
        &["_mermaid", "_mermaid_group"],
    ))
}

/// Scan a CSV file into a header block plus row-range blocks.
fn scan_csv_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_csv(bytes)?;
    Ok(manifest_from_optimized(result, &["_csv_header"]))
}

/// Scan a ZIP archive into one block per entry.
fn scan_zip_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_zip(bytes)?;
    Ok(manifest_from_optimized(result, &[]))
}

/// Scan an HTML file into title/heading-delimited section blocks.
fn scan_html_manifest(_path: &str, bytes: &[u8]) -> Result<ScanResult> {
    let result = handlers::optimized::scan_html(bytes)?;
    Ok(manifest_from_optimized(
        result,
        &["_html_title", "_html_section"],
    ))
}

/// Decompose an OOXML table into row and cell blocks.
fn decompose_table(
    table_node: &DocNode,
    _file_type: FileType,
    blocks: &mut Vec<BlockManifest>,
    section_counter: &mut usize,
    parent_id: &Option<String>,
    index: &mut Option<HashMap<String, DocNode>>,
) {
    // Add the table block itself
    let table_id = format!(
        "table[{}]",
        blocks
            .iter()
            .filter(|b| b.block_type == BlockType::Table)
            .count()
    );
    if let Some(map) = index.as_mut() {
        map.insert(table_id.clone(), table_node.clone());
    }
    let table_preview = format!(
        "Table ({} rows)",
        table_node
            .children
            .iter()
            .filter(|c| c.tag == "w:tr" || c.tag == "a:tr")
            .count()
    );
    let table_hash = table_node.id.clone();

    blocks.push(BlockManifest {
        id: table_id.clone(),
        block_type: BlockType::Table,
        preview: table_preview,
        content_hash: table_hash,
        parent_id: parent_id.clone(),
        token_estimate: estimate_tokens(&table_node.text.clone().unwrap_or_default()),
        section_name: String::new(),
        section_number: *section_counter,
    });

    // Add row and cell blocks
    for (row_idx, row_node) in table_node.children.iter().enumerate() {
        if row_node.tag != "w:tr" && row_node.tag != "a:tr" {
            continue;
        }

        let row_id = format!("{}.row[{}]", table_id, row_idx);
        if let Some(map) = index.as_mut() {
            map.insert(row_id.clone(), row_node.clone());
        }
        let row_hash = row_node.id.clone();
        let row_cell_count = row_node
            .children
            .iter()
            .filter(|c| c.tag == "w:tc" || c.tag == "a:tc")
            .count();

        blocks.push(BlockManifest {
            id: row_id.clone(),
            block_type: BlockType::List,
            preview: format!("Row {} ({} cells)", row_idx + 1, row_cell_count),
            content_hash: row_hash,
            parent_id: Some(table_id.clone()),
            token_estimate: 0,
            section_name: String::new(),
            section_number: *section_counter,
        });

        // Add cell blocks
        for (cell_idx, cell_node) in row_node.children.iter().enumerate() {
            if cell_node.tag != "w:tc" && cell_node.tag != "a:tc" {
                continue;
            }

            let cell_id = format!("{}.cell[{}]", row_id, cell_idx);
            if let Some(map) = index.as_mut() {
                map.insert(cell_id.clone(), cell_node.clone());
            }
            let cell_hash = cell_node.id.clone();
            let cell_text = extract_cell_text(cell_node);
            let cell_preview = if cell_text.is_empty() {
                "<empty>".to_string()
            } else {
                truncate_ellipsis(&cell_text, 50)
            };

            blocks.push(BlockManifest {
                id: cell_id,
                block_type: BlockType::Cell,
                preview: cell_preview,
                content_hash: cell_hash,
                parent_id: Some(row_id.clone()),
                token_estimate: estimate_tokens(&cell_text),
                section_name: String::new(),
                section_number: *section_counter,
            });
        }
    }
}

/// Extract text content from a table cell.
/// After merge_runs, paragraph text is in the w:p element's text field.
fn extract_cell_text(node: &DocNode) -> String {
    // After merge_runs, paragraph text is in the text field
    // Look for w:p children and get their text
    let mut text = String::new();
    for child in &node.children {
        if child.tag == "w:p" || child.tag == "a:p" {
            if let Some(t) = &child.text {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(t);
            }
        }
    }
    text.trim().to_string()
}

/// Generate a structural path ID based on file type, node position, and semantics.
fn generate_structural_id(
    node: &DocNode,
    idx: usize,
    file_type: FileType,
    section_counter: &usize,
    blocks: &[BlockManifest],
    _parent_id: &Option<String>,
) -> String {
    match file_type {
        FileType::Ooxml => {
            let block_type = map_block_type(node, file_type);
            match node.tag.as_str() {
                "w:p" | "a:p" => {
                    if block_type == BlockType::Heading {
                        format!("heading[{}]", section_counter)
                    } else {
                        // Count paragraphs (excluding headings)
                        let para_count = blocks
                            .iter()
                            .filter(|b| b.block_type == BlockType::Paragraph)
                            .count();
                        format!("paragraph[{}]", para_count)
                    }
                }
                "w:tbl" | "a:tbl" => {
                    let table_count = blocks
                        .iter()
                        .filter(|b| b.block_type == BlockType::Table)
                        .count();
                    format!("table[{}]", table_count)
                }
                "w:tr" | "a:tr" => format!("row[{}]", idx),
                "w:tc" | "a:tc" => format!("cell[{}]", idx),
                "w:t" | "a:t" | "t" => format!("text[{}]", idx),
                "_part" => {
                    // Part marker - use part name as ID
                    node.attrs
                        .iter()
                        .find(|a| a.name == "name")
                        .map(|a| format!("part:{}", a.value))
                        .unwrap_or_else(|| format!("part[{}]", idx))
                }
                _ => format!("element[{}]", idx),
            }
        }
        FileType::Xml | FileType::Drawio => {
            format!("node[{}:{}]", node.tag, idx)
        }
        _ => {
            format!("block[{}]", idx)
        }
    }
}

/// Extract a preview string from a node (first ~100 chars).
fn extract_preview(node: &DocNode, block_type: BlockType) -> String {
    match block_type {
        BlockType::Heading => {
            // For headings, include the heading level if available
            if let Some(text) = &node.text {
                format!("Heading: {}", truncate_ellipsis(text, 80))
            } else {
                "Heading".to_string()
            }
        }
        BlockType::Table => {
            // For tables, show dimensions
            let rows = node
                .children
                .iter()
                .filter(|c| c.tag == "w:tr" || c.tag == "a:tr")
                .count();
            let cols = node
                .children
                .first()
                .map(|r| {
                    r.children
                        .iter()
                        .filter(|c| c.tag == "w:tc" || c.tag == "a:tc")
                        .count()
                })
                .unwrap_or(0);
            format!("Table ({}x{})", rows, cols)
        }
        BlockType::List => {
            // For list items (rows), show item count
            let items = node
                .children
                .iter()
                .filter(|c| c.tag == "w:tc" || c.tag == "a:tc")
                .count();
            format!("List ({} items)", items)
        }
        _ => {
            // Default: first ~100 chars of text
            if let Some(text) = &node.text {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    "<empty>".to_string()
                } else {
                    truncate_ellipsis(trimmed, 100)
                }
            } else if node.children.is_empty() {
                "<empty>".to_string()
            } else {
                format!("<{} with {} children>", node.tag, node.children.len())
            }
        }
    }
}

/// Truncate `text` to at most `max` characters, appending an ellipsis when
/// truncation occurs. Operates on character boundaries, so it never panics on
/// multi-byte UTF-8.
fn truncate_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let head: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{head}...")
    } else {
        text.to_string()
    }
}

/// Estimate token count from text.
/// Uses word count as a rough heuristic (1 word ≈ 1.3 tokens).
fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // Count words by splitting on whitespace
    let word_count = text.split_whitespace().count();
    // Rough heuristic: 1 word ≈ 1.3 tokens, with some overhead for punctuation
    (word_count as f64 * 1.3) as usize + 1
}

/// Map handler type name to FileType enum.
fn map_file_type(type_name: &str) -> FileType {
    match type_name {
        "markdown" | "md" => FileType::Markdown,
        "docx" | "xlsx" | "pptx" => FileType::Ooxml,
        "xml" | "svg" | "xhtml" => FileType::Xml,
        "drawio" | "dio" => FileType::Drawio,
        "pdf" => FileType::Pdf,
        _ => FileType::Unknown,
    }
}

/// Map XML tag to BlockType based on file type and node.
fn map_block_type(node: &DocNode, file_type: FileType) -> BlockType {
    match file_type {
        FileType::Ooxml => {
            match node.tag.as_str() {
                "w:p" | "a:p" => {
                    // Check for heading style in w:pPr child or direct attributes
                    if node_is_heading(node) || is_heading_paragraph(&node.attrs) {
                        BlockType::Heading
                    } else {
                        BlockType::Paragraph
                    }
                }
                "w:tbl" | "a:tbl" => BlockType::Table,
                "w:tr" | "a:tr" => BlockType::List, // rows are list-like
                "w:tc" | "a:tc" => BlockType::Cell,
                "w:t" | "a:t" | "t" => BlockType::Other, // text runs
                "_part" => BlockType::Section,
                _ => BlockType::Other,
            }
        }
        FileType::Xml | FileType::Drawio => {
            if node.tag.contains("text") || node.tag == "mxCell" {
                BlockType::Paragraph
            } else if node.tag.contains("table") || node.tag == "mxGraphModel" {
                BlockType::Table
            } else {
                BlockType::Other
            }
        }
        _ => BlockType::Other,
    }
}

/// Check if a paragraph has a heading style.
/// OOXML headings use w:pStyle with values like "Heading1", "Heading2", etc.
/// The style is in a child w:pPr element, not a direct attribute.
fn is_heading_paragraph(attrs: &[Attr]) -> bool {
    attrs.iter().any(|a| {
        a.name == "w:pStyle"
            && (a.value.starts_with("Heading")
                || a.value.starts_with("heading")
                || a.value == "Title"
                || a.value == "Subtitle")
    })
}

/// Check if a DocNode is a heading by examining its descendants for w:pStyle.
/// The structure is: w:p -> w:pPr -> w:pStyle w:val="Heading1"
fn node_is_heading(node: &DocNode) -> bool {
    for child in &node.children {
        if child.tag == "w:pPr" {
            // Check w:pPr's children for w:pStyle
            for grandchild in &child.children {
                if grandchild.tag == "w:pStyle" {
                    // Check the w:val attribute
                    if grandchild.attrs.iter().any(|a| {
                        a.name == "w:val"
                            && (a.value.starts_with("Heading")
                                || a.value.starts_with("heading")
                                || a.value == "Title"
                                || a.value == "Subtitle")
                    }) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Clear all caches (scan and extract).
pub fn clear_scan_cache() {
    let mut cache = scan_cache();
    *cache = None;
    let mut extract_cache = extract_cache();
    *extract_cache = None;
}

/// Read specific blocks from a file by their structural path IDs.
///
/// Returns only the requested blocks with full content hydrated.
/// If ids is empty, returns all blocks (backwards compatibility).
///
/// For XLSX files, supports row-range syntax:
///   - "sheet[0]" → returns sheet metadata
///   - "sheet[0].rows[0-99]" → loads cells for rows 0-99
///   - "sheet[0].cell[0,0]" → loads specific cell
pub fn read(path: &str, bytes: &[u8], ids: &[String]) -> Result<Vec<DocNode>> {
    // For XLSX, use optimized path that avoids full cell parsing
    if path.ends_with(".xlsx") {
        return read_xlsx(bytes, ids);
    }

    // Formats with an optimized scan return their block tree directly so that
    // `read` ids line up with `scan` ids (no second, divergent extraction).
    if let Some(structure) = optimized_structure(path, bytes)? {
        if ids.is_empty() {
            return Ok(structure);
        }
        return Ok(structure
            .into_iter()
            .filter(|n| ids.iter().any(|id| id == &n.id))
            .collect());
    }

    let extract_out = extract(path, bytes)?;
    let file_type = map_file_type(&extract_out.envelope.source.r#type);

    if ids.is_empty() {
        // Return all blocks
        return Ok(extract_out.envelope.structure);
    }

    // Build the same id -> node index that `scan()` produces, so requested ids
    // resolve to exactly the blocks the manifest advertised.
    let mut blocks = Vec::new();
    let mut section_counter = 0usize;
    let mut index: Option<HashMap<String, DocNode>> = Some(HashMap::new());
    build_manifest(
        &extract_out.envelope.structure,
        file_type,
        &mut blocks,
        &mut section_counter,
        None,
        &mut index,
    );
    let node_map = index.unwrap_or_default();

    // Collect requested nodes
    let mut result = Vec::new();
    for id in ids {
        if let Some(node) = node_map.get(id) {
            result.push(node.clone());
        }
    }

    Ok(result)
}

/// Search a file's text content for `needle`, returning the ids of matching
/// blocks plus a line/snippet for context.
///
/// This is the discovery counterpart to `read`: instead of hydrating every
/// block to find the interesting ones (token-expensive), `grep` walks the same
/// block tree `read` produces and returns only `GrepMatch`es. Each `block_id`
/// resolves directly via `read`/`reconstruct`, so a caller funnels matches
/// straight into an edit. `writable` flags whether the format can be patched.
///
/// `needle` is matched as a literal substring (per line of a block's text).
pub fn grep(path: &str, bytes: &[u8], needle: &str, opts: &GrepOptions) -> Result<Vec<GrepMatch>> {
    let writable = is_writable_format(path, bytes);

    let hay_needle = if opts.ignore_case {
        needle.to_lowercase()
    } else {
        needle.to_string()
    };

    // XLSX cells aren't surfaced by the lightweight scan structure, so grep
    // hydrates each row range on demand and reports the range id that `read`
    // accepts — mirroring the CSV handler's `rows[a-b]` block addressing.
    if path.ends_with(".xlsx") {
        return grep_xlsx(bytes, &hay_needle, opts, writable);
    }

    let nodes = read(path, bytes, &[])?;
    let mut matches = Vec::new();
    grep_walk(&nodes, &hay_needle, opts, writable, &mut matches);
    Ok(matches)
}

/// Grep an xlsx workbook: match sheet-name blocks directly, then hydrate each
/// row range and match its resolved cell values, attributing every cell hit to
/// the range's block id so the caller can hydrate it via `read`.
fn grep_xlsx(
    bytes: &[u8],
    needle: &str,
    opts: &GrepOptions,
    writable: bool,
) -> Result<Vec<GrepMatch>> {
    let result = handlers::xlsx::scan_xlsx(bytes)?;
    let mut matches = Vec::new();

    let hit = |hay: &str| -> bool {
        if opts.ignore_case {
            hay.to_lowercase().contains(needle)
        } else {
            hay.contains(needle)
        }
    };

    for node in &result.structure {
        if let Some(limit) = opts.limit {
            if matches.len() >= limit {
                return Ok(matches);
            }
        }

        match node.tag.as_str() {
            "_sheet" => {
                if let Some(text) = &node.text {
                    if hit(text) {
                        matches.push(GrepMatch {
                            block_id: node.id.clone(),
                            line: 1,
                            snippet: truncate_ellipsis(text.trim(), 120),
                            writable,
                        });
                    }
                }
            }
            "_row_range" => {
                let sheet_idx: usize = attr(node, "sheet_idx").and_then(|v| v.parse().ok()).unwrap_or(0);
                let sheet_part = match attr(node, "sheet") {
                    Some(p) => p,
                    None => continue,
                };
                let start: usize = attr(node, "start").and_then(|v| v.parse().ok()).unwrap_or(0);
                let end: usize = attr(node, "end").and_then(|v| v.parse().ok()).unwrap_or(0);

                let rows =
                    handlers::xlsx::load_row_range(bytes, sheet_part, sheet_idx, start, end)?;
                for row in &rows {
                    for cell in &row.children {
                        let Some(text) = &cell.text else { continue };
                        if text.is_empty() {
                            continue;
                        }
                        if hit(text) {
                            matches.push(GrepMatch {
                                block_id: node.id.clone(),
                                line: 1,
                                snippet: truncate_ellipsis(text.trim(), 120),
                                writable,
                            });
                            if opts.limit.is_some_and(|l| matches.len() >= l) {
                                return Ok(matches);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(matches)
}

/// First value of attribute `name` on a node.
fn attr<'a>(node: &'a DocNode, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.as_str())
}

/// Recursively scan `nodes`, pushing a `GrepMatch` per matching line of each
/// node's direct text. Honors `opts.limit` (stops once reached).
fn grep_walk(
    nodes: &[DocNode],
    needle: &str,
    opts: &GrepOptions,
    writable: bool,
    out: &mut Vec<GrepMatch>,
) {
    for node in nodes {
        if let Some(limit) = opts.limit {
            if out.len() >= limit {
                return;
            }
        }
        if let Some(text) = &node.text {
            for (i, line) in text.lines().enumerate() {
                let hay = if opts.ignore_case {
                    line.to_lowercase()
                } else {
                    line.to_string()
                };
                if hay.contains(needle) {
                    out.push(GrepMatch {
                        block_id: node.id.clone(),
                        line: i + 1,
                        snippet: truncate_ellipsis(line.trim(), 120),
                        writable,
                    });
                    if opts.limit.is_some_and(|l| out.len() >= l) {
                        return;
                    }
                }
            }
        }
        grep_walk(&node.children, needle, opts, writable, out);
    }
}

/// Whether a path's format can be edited and reconstructed (has a non-read-only
/// handler). Cheap: inspects the handler's declared fidelity, no extraction.
fn is_writable_format(path: &str, bytes: &[u8]) -> bool {
    handlers::for_path(path, bytes).fidelity() != Fidelity::ReadOnly
}

/// Return the optimized block tree for formats that have a dedicated scan
/// handler, or `None` for formats that should use generic extraction.
///
/// Keeping this in lockstep with `scan_uncached` guarantees `read` ids match
/// the ids `scan` reported.
fn optimized_structure(path: &str, bytes: &[u8]) -> Result<Option<Vec<DocNode>>> {
    let result = if path.ends_with(".pptx") {
        handlers::optimized::scan_pptx(bytes)?
    } else if path.ends_with(".pdf") {
        handlers::optimized::scan_pdf(bytes)?
    } else if path.ends_with(".svg") {
        handlers::optimized::scan_svg(bytes)?
    } else if path.ends_with(".drawio") || path.ends_with(".dio") {
        handlers::optimized::scan_drawio(bytes)?
    } else if path.ends_with(".md") || path.ends_with(".markdown") {
        handlers::optimized::scan_markdown(bytes)?
    } else if path.ends_with(".mmd") || path.ends_with(".mermaid") {
        handlers::optimized::scan_mermaid(bytes)?
    } else if path.ends_with(".csv") {
        handlers::optimized::scan_csv(bytes)?
    } else if path.ends_with(".zip") {
        handlers::optimized::scan_zip(bytes)?
    } else if path.ends_with(".html") || path.ends_with(".htm") {
        handlers::optimized::scan_html(bytes)?
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") || path.ends_with(".png") {
        handlers::optimized::scan_binary(path, bytes)?
    } else {
        return Ok(None);
    };
    Ok(Some(result.structure))
}

/// Read XLSX blocks using optimized path.
/// For row ranges, loads only the requested cells.
fn read_xlsx(bytes: &[u8], ids: &[String]) -> Result<Vec<DocNode>> {
    if ids.is_empty() {
        // Return sheet-level blocks
        let xlsx_result = handlers::xlsx::scan_xlsx(bytes)?;
        return Ok(xlsx_result.structure);
    }

    let mut result = Vec::new();
    // Lazily computed sheet-level structure, shared across non-range ids.
    let mut sheet_structure: Option<Vec<DocNode>> = None;

    for id in ids {
        // Check if this is a row range request
        if id.contains(".rows[") {
            if let Some(nodes) = load_row_range_nodes(bytes, id)? {
                result.extend(nodes);
            }
        } else {
            // Sheet-level block - return metadata
            let structure = match &sheet_structure {
                Some(s) => s,
                None => sheet_structure.insert(handlers::xlsx::scan_xlsx(bytes)?.structure),
            };
            if let Some(node) = structure.iter().find(|n| n.id == *id) {
                result.push(node.clone());
            }
        }
    }

    Ok(result)
}

/// Load cells for a row range request.
/// Parses the ID format "sheet[0].rows[0-99]" and loads the corresponding cells.
fn load_row_range_nodes(bytes: &[u8], id: &str) -> Result<Option<Vec<DocNode>>> {
    // Parse the ID: "sheet[0].rows[0-99]"
    let parts: Vec<&str> = id.split(".rows[").collect();
    if parts.len() != 2 {
        return Ok(None);
    }

    let sheet_id = parts[0];
    let range_str = parts[1].trim_end_matches(']');

    // Parse range: "0-99"
    let range_parts: Vec<&str> = range_str.split('-').collect();
    if range_parts.len() != 2 {
        return Ok(None);
    }

    // Range ids are inclusive on both ends ("0-99" → rows 0..=99).
    let start_row: usize = range_parts[0].parse().unwrap_or(0);
    let end_row: usize = range_parts[1].parse().unwrap_or(0);

    // Extract sheet index from sheet_id: "sheet[0]" → 0
    let sheet_idx_str = sheet_id.trim_start_matches("sheet[").trim_end_matches(']');
    let sheet_idx: usize = sheet_idx_str.parse().unwrap_or(0);

    // Resolve the worksheet part the same way `scan_xlsx` orders them, so a
    // range id minted by scan addresses the same sheet here.
    let sheet_part = match handlers::xlsx::scan_xlsx(bytes)?
        .sheets
        .get(sheet_idx)
        .map(|s| s.part_name.clone())
    {
        Some(part) => part,
        None => return Ok(None),
    };

    let nodes =
        handlers::xlsx::load_row_range(bytes, &sheet_part, sheet_idx, start_row, end_row)?;

    Ok(Some(nodes))
}

/// Write an envelope back to a file.
///
/// This is the public API for reconstruction. Verifies the original hash,
/// applies patches, and writes the result.
pub fn write(
    envelope: &Envelope,
    idmap: &IdMap,
    original: &[u8],
    patch: &Patch,
) -> Result<Vec<u8>> {
    reconstruct(envelope, idmap, original, patch)
}

/// Apply patches to an envelope.
///
/// This is the public API for editing. Patches are applied to the in-memory
/// representation. Use `write()` to persist changes.
pub fn edit(envelope: &Envelope, idmap: &IdMap, original: &[u8], patch: &Patch) -> Result<Vec<u8>> {
    // Validate all guards before applying
    for op in &patch.patch {
        if let patch::Op::Test { path, hash } = op {
            let id = path.strip_prefix("/structure/").unwrap_or(path);
            if let Some(loc) = idmap.get(id) {
                if &loc.hash != hash {
                    bail!(
                        "guard failed for `{}`: expected {}, found {}",
                        id,
                        hash,
                        loc.hash
                    );
                }
            }
        }
    }
    reconstruct(envelope, idmap, original, patch)
}

/// Apply a patch to the original and return the reconstructed bytes.
///
/// Refuses non-writable envelopes and confirms the sidecar belongs to this
/// original before splicing. When the original still matches the extract hash
/// this splices directly; when it has drifted (an out-of-band rewrite) it hands
/// off to `reconstruct_drifted` for autonomous, fingerprint-based recovery.
fn reconstruct(
    envelope: &Envelope,
    idmap: &IdMap,
    original: &[u8],
    patch: &Patch,
) -> Result<Vec<u8>> {
    if !envelope.writable {
        bail!(
            "envelope is read-only (fidelity {:?}); cannot reconstruct",
            envelope.fidelity
        );
    }
    let actual = sha256_hex(original);
    if actual != envelope.source.hash {
        // The original was rewritten out-of-band (e.g. openpyxl) since extract.
        // Autonomously recover: re-extract the current bytes and re-target the
        // patch by content fingerprint, then apply against the fresh id-map.
        return reconstruct_drifted(envelope, idmap, original, patch);
    }
    if idmap.for_hash != envelope.source.hash {
        bail!("sidecar id-map does not match this original (hash mismatch)");
    }
    let handler = handlers::for_type(&envelope.source.r#type)
        .with_context(|| format!("no handler for type `{}`", envelope.source.r#type))?;
    handler.reconstruct(original, idmap, patch)
}

/// A *semantic* content fingerprint for a patch target: its own normalized
/// text signature plus its parent's. Parent-anchoring disambiguates the common
/// case of repeated content (identical cells/paragraphs) so a re-target lands
/// on the node the patch *meant*, not merely one that looks the same.
///
/// Unlike a byte hash, the signature survives whitespace reflow, attribute
/// reordering, entity reencoding, and wholesale serializer rewrites (e.g.
/// openpyxl re-emitting an xlsx) — because it keys on the preserved *value*,
/// not the exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Fingerprint {
    signature: String,
    parent_signature: Option<String>,
}

/// Collapse whitespace runs to single spaces and trim, so reflow/reindent and
/// entity differences don't change a node's text identity.
fn normalize_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Depth-first, document-order concatenation of a node's own and descendant
/// text. Container nodes (no own text) are identified by the text they enclose.
fn collect_text(node: &DocNode, out: &mut String) {
    if let Some(t) = &node.text {
        out.push_str(t);
        out.push(' ');
    }
    for c in &node.children {
        collect_text(c, out);
    }
}

/// Semantic signature of a node: its tag plus its normalized descendant text.
/// The tag guards against a text-equal node of a different kind colliding.
fn node_signature(node: &DocNode) -> String {
    let mut text = String::new();
    collect_text(node, &mut text);
    // U+001F (unit separator) cannot appear in a tag, so it cleanly delimits.
    format!("{}\u{1f}{}", node.tag, normalize_text(&text))
}

/// Fingerprint every node in a structure tree (id -> semantic fingerprint).
fn fingerprint_tree(structure: &[DocNode]) -> HashMap<String, Fingerprint> {
    fn walk(nodes: &[DocNode], parent_sig: Option<&str>, out: &mut HashMap<String, Fingerprint>) {
        for node in nodes {
            let sig = node_signature(node);
            out.insert(
                node.id.clone(),
                Fingerprint {
                    signature: sig.clone(),
                    parent_signature: parent_sig.map(str::to_string),
                },
            );
            walk(&node.children, Some(&sig), out);
        }
    }
    let mut out = HashMap::new();
    walk(structure, None, &mut out);
    out
}

/// Fingerprint every distinct id an `id`-based op references, using the
/// structure tree the patch was authored against.
fn fingerprint_targets(
    structure: &[DocNode],
    patch: &Patch,
) -> Result<HashMap<String, Fingerprint>> {
    let all = fingerprint_tree(structure);
    let mut out: HashMap<String, Fingerprint> = HashMap::new();
    for op in &patch.patch {
        let Some(id) = op.target_id() else { continue };
        if out.contains_key(id) {
            continue;
        }
        let fp = all.get(id).with_context(|| {
            format!("patch references id `{id}` absent from the structure it was authored against")
        })?;
        out.insert(id.to_string(), fp.clone());
    }
    Ok(out)
}

/// Build the reverse index from `Fingerprint` to id for a (fresh) structure.
/// Fingerprints that are not unique map to `None`, so an ambiguous re-target
/// fails loud instead of editing an arbitrary match.
fn index_by_fingerprint(structure: &[DocNode]) -> HashMap<Fingerprint, Option<String>> {
    let mut idx: HashMap<Fingerprint, Option<String>> = HashMap::new();
    for (id, fp) in fingerprint_tree(structure) {
        idx.entry(fp)
            .and_modify(|slot| *slot = None) // collision -> ambiguous
            .or_insert(Some(id));
    }
    idx
}

/// Rewrite an op's target/anchor id from `old` to `new`.
fn retarget_op(op: &patch::Op, old: &str, new: &str) -> patch::Op {
    let swap_path = |path: &str| -> String {
        match path.strip_prefix("/structure/") {
            Some(rest) => {
                let tail = rest
                    .strip_prefix(old)
                    .filter(|t| t.is_empty() || t.starts_with('/'));
                match tail {
                    Some(t) => format!("/structure/{new}{t}"),
                    None => path.to_string(),
                }
            }
            None => path.to_string(),
        }
    };
    match op {
        patch::Op::Test { path, hash } => patch::Op::Test {
            path: swap_path(path),
            hash: hash.clone(),
        },
        patch::Op::Replace { path, value } => patch::Op::Replace {
            path: swap_path(path),
            value: value.clone(),
        },
        patch::Op::Remove { path } => patch::Op::Remove {
            path: swap_path(path),
        },
        patch::Op::Add {
            after,
            before,
            value,
        } => patch::Op::Add {
            after: after.as_deref().map(|a| {
                if a == old {
                    new.to_string()
                } else {
                    a.to_string()
                }
            }),
            before: before.as_deref().map(|b| {
                if b == old {
                    new.to_string()
                } else {
                    b.to_string()
                }
            }),
            value: value.clone(),
        },
    }
}

/// Autonomous drift recovery (Preset B): re-extract the rewritten original,
/// re-target every op by parent-anchored content fingerprint, and apply the
/// rewritten patch atomically against the fresh id-map.
///
/// Refuses (fails loud, applies nothing) when any target's fingerprint is gone
/// or ambiguous in the new extract — the only safe response when the content
/// the patch meant to edit can no longer be located unambiguously.
fn reconstruct_drifted(
    envelope: &Envelope,
    idmap: &IdMap,
    original: &[u8],
    patch: &Patch,
) -> Result<Vec<u8>> {
    if idmap.for_hash != envelope.source.hash {
        bail!("sidecar id-map does not match this original (hash mismatch)");
    }

    // What each op meant, fingerprinted against the OLD structure it was
    // authored on (semantic text signatures, parent-anchored).
    let want = fingerprint_targets(&envelope.structure, patch)?;

    // Re-extract the CURRENT bytes and index them by fingerprint.
    let fresh = extract(&envelope.source.path, original)
        .context("drift recovery: re-extracting the rewritten original failed")?;
    let fresh_idmap = fresh
        .idmap
        .as_ref()
        .context("drift recovery: rewritten original is not a writable format")?;
    let fresh_index = index_by_fingerprint(&fresh.envelope.structure);

    // Resolve old id -> new id, failing loud on any miss or ambiguity.
    let mut remap: HashMap<String, String> = HashMap::new();
    for (old_id, fp) in &want {
        match fresh_index.get(fp) {
            Some(Some(new_id)) => {
                remap.insert(old_id.clone(), new_id.clone());
            }
            Some(None) => bail!(
                "drift recovery: target `{old_id}` is ambiguous after re-extract \
                 (its content and parent now match multiple nodes); aborting without edit"
            ),
            None => bail!(
                "drift recovery: target `{old_id}` no longer exists after the original \
                 was rewritten; aborting without edit"
            ),
        }
    }

    // Signal the autonomous recovery so a caller isn't silently surprised that
    // re-targeting happened (the file changed out-of-band since extract).
    eprintln!(
        "drift recovery: original `{}` was rewritten since extract; \
         re-targeted {} of {} ops by content fingerprint",
        envelope.source.path,
        remap.len(),
        patch.patch.len(),
    );

    // Rewrite the patch onto fresh ids and apply atomically.
    let rewritten = Patch {
        patch: patch
            .patch
            .iter()
            .map(|op| match op.target_id() {
                Some(id) => match remap.get(id) {
                    Some(new) => retarget_op(op, id, new),
                    None => op.clone(),
                },
                None => op.clone(),
            })
            .collect(),
    };

    let handler = handlers::for_type(&fresh.envelope.source.r#type)
        .with_context(|| format!("no handler for type `{}`", fresh.envelope.source.r#type))?;
    handler.reconstruct(original, fresh_idmap, &rewritten)
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
