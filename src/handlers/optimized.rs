//! Optimized handlers for various file formats.
//!
//! Provides hierarchical block structures to reduce block count and improve
//! scan/read performance for large files.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use zip::ZipArchive;

use crate::model::{Attr, DocNode, FileType};

/// Optimized scan result for any format.
pub struct OptimizedScanResult {
    pub file_type: FileType,
    pub structure: Vec<DocNode>,
    pub block_count: usize,
    pub total_tokens: usize,
}

/// Scan PPTX with slide-level blocks, reading slide parts straight from the
/// zip container. This avoids the full OOXML element parse: each slide block is
/// derived from a single decompressed part and a cheap `<a:t>` text scan.
pub fn scan_pptx(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let mut slide_parts = zip_entry_names(bytes)?
        .into_iter()
        .filter(|n| {
            n.starts_with("ppt/slides/slide") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .collect::<Vec<_>>();
    // Order slides numerically (slide2 before slide10).
    slide_parts.sort_by_key(|n| slide_number(n));

    let mut structure = Vec::new();
    let mut total_tokens = 0;

    for (idx, part) in slide_parts.iter().enumerate() {
        let part_bytes = zip_read_part(bytes, part)?;
        let content = String::from_utf8_lossy(&part_bytes);
        let (run_count, preview) = scan_drawingml_text(&content);

        let text = if preview.is_empty() {
            format!("Slide {} ({} text runs)", idx + 1, run_count)
        } else {
            format!("Slide {}: {}", idx + 1, preview_text(&preview))
        };

        // The slide block keeps a truncated preview as its own text (so the
        // scan manifest stays lightweight), while its full paragraph text is
        // carried as children so `read`/`grep` can reach every run, not just
        // the preview.
        let children = slide_paragraphs(&content)
            .into_iter()
            .enumerate()
            .map(|(p, para)| DocNode {
                id: format!("slide[{idx}].p[{p}]"),
                tag: "_slide_text".to_string(),
                attrs: Vec::new(),
                text: Some(para),
                children: Vec::new(),
            })
            .collect();

        structure.push(DocNode {
            id: format!("slide[{idx}]"),
            tag: "_slide".to_string(),
            attrs: vec![
                Attr {
                    name: "part".to_string(),
                    value: part.clone(),
                },
                Attr {
                    name: "runs".to_string(),
                    value: run_count.to_string(),
                },
            ],
            text: Some(text),
            children,
        });
        total_tokens += estimate_node_tokens(structure.last().unwrap());
    }

    let block_count = structure.len();
    Ok(OptimizedScanResult {
        file_type: FileType::Ooxml,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan PDF with page-level blocks.
/// Each page becomes a single block with text preview.
pub fn scan_pdf(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let extract_out = crate::extract("pdf", bytes)?;

    let mut structure = Vec::new();
    let mut total_tokens = 0;

    // Chunk text nodes into fixed-size page blocks. The extractor does not
    // expose true page boundaries, so we group every CHUNK nodes as one page
    // and flush any trailing partial page.
    const CHUNK: usize = 50;
    let nodes = &extract_out.envelope.structure;
    for (page_idx, chunk) in nodes.chunks(CHUNK).enumerate() {
        let page_num = page_idx + 1;
        let preview = chunk
            .iter()
            .filter_map(|n| n.text.as_ref())
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");

        let page_preview = if preview.is_empty() {
            format!("Page {page_num}")
        } else {
            preview_text(&preview)
        };

        structure.push(DocNode {
            id: format!("page[{page_num}]"),
            tag: "_page".to_string(),
            attrs: vec![
                Attr {
                    name: "page".to_string(),
                    value: page_num.to_string(),
                },
                Attr {
                    name: "blocks".to_string(),
                    value: chunk.len().to_string(),
                },
            ],
            text: Some(page_preview),
            children: chunk.to_vec(), // Keep children for lazy loading
        });

        total_tokens += estimate_node_tokens(structure.last().unwrap());
    }

    let block_count = structure.len();
    Ok(OptimizedScanResult {
        file_type: FileType::Pdf,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan SVG with element-level blocks.
/// Groups elements by type (paths, text, groups).
pub fn scan_svg(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let extract_out = crate::extract("svg", bytes)?;

    let mut structure = Vec::new();
    let mut block_count = 0;

    // Group elements by type
    let mut groups = BTreeMap::new();

    for node in &extract_out.envelope.structure {
        let category = categorize_svg_element(&node.tag);
        groups
            .entry(category)
            .or_insert_with(Vec::new)
            .push(node.clone());
    }

    // Create category-level blocks
    for (category, nodes) in groups {
        let preview = format!("{} elements", nodes.len());

        structure.push(DocNode {
            id: format!("svg:{}", category),
            tag: "_svg_group".to_string(),
            attrs: vec![
                Attr {
                    name: "category".to_string(),
                    value: category.clone(),
                },
                Attr {
                    name: "count".to_string(),
                    value: nodes.len().to_string(),
                },
            ],
            text: Some(preview),
            children: nodes,
        });

        block_count += 1;
    }

    let total_tokens = structure.iter().map(estimate_node_tokens).sum();

    Ok(OptimizedScanResult {
        file_type: FileType::Xml,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan Drawio with diagram-level blocks.
/// Each diagram becomes a single block.
pub fn scan_drawio(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let extract_out = crate::extract("drawio", bytes)?;

    let mut structure = Vec::new();
    let mut block_count = 0;
    let mut total_tokens = 0;

    // Group nodes by diagram (part marker)
    for node in &extract_out.envelope.structure {
        if node.tag == "_diagram" || node.tag == "_part" {
            // This is a diagram marker
            let diagram_name = node
                .attrs
                .iter()
                .find(|a| a.name == "name")
                .map(|a| a.value.clone())
                .unwrap_or_else(|| format!("diagram {}", structure.len() + 1));

            // Count cells in this diagram
            let mut cell_count = 0;
            count_nodes(&node.children, &mut cell_count, &mut total_tokens);

            // Create diagram-level block, keeping the cells as children so
            // grep/read reach cell labels (drawio stores labels in the `value`
            // attribute, not element text).
            structure.push(DocNode {
                id: format!("diagram:{}", diagram_name),
                tag: "_diagram_block".to_string(),
                attrs: vec![
                    Attr {
                        name: "name".to_string(),
                        value: diagram_name,
                    },
                    Attr {
                        name: "cells".to_string(),
                        value: cell_count.to_string(),
                    },
                ],
                text: Some(format!("Diagram ({} cells)", cell_count)),
                children: node.children.clone(),
            });

            block_count += 1;
        }
    }

    // Add any non-diagram nodes
    for node in &extract_out.envelope.structure {
        if node.tag != "_diagram" && node.tag != "_part" {
            structure.push(node.clone());
            block_count += 1;
            total_tokens += estimate_node_tokens(node);
        }
    }

    Ok(OptimizedScanResult {
        file_type: FileType::Drawio,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan Markdown with section-level blocks.
/// Groups content by headings.
pub fn scan_markdown(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let content = std::str::from_utf8(bytes).with_context(|| "invalid UTF-8 in markdown")?;

    let mut structure = Vec::new();
    let mut block_count = 0;
    let mut total_tokens = 0;

    let mut current_section = String::new();
    let mut current_content = Vec::new();
    let mut section_num = 0;

    for line in content.lines() {
        if line.starts_with('#') {
            // Save previous section
            if !current_section.is_empty() || !current_content.is_empty() {
                let preview = current_content
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");

                let section_preview = if preview.is_empty() {
                    format!("Section {}", section_num)
                } else {
                    preview_text(&preview)
                };

                structure.push(DocNode {
                    id: format!("section[{}]", section_num),
                    tag: "_section".to_string(),
                    attrs: vec![Attr {
                        name: "heading".to_string(),
                        value: current_section.clone(),
                    }],
                    text: Some(section_preview),
                    children: markdown_section_children(
                        section_num,
                        &current_section,
                        &current_content,
                    ),
                });

                block_count += 1;
                total_tokens += estimate_node_tokens(structure.last().unwrap());

                section_num += 1;
                current_content = Vec::new();
            }

            // Extract heading text
            current_section = line.trim_start_matches('#').trim().to_string();
        } else if !line.trim().is_empty() {
            current_content.push(line.to_string());
        }
    }

    // Add final section
    if !current_section.is_empty() || !current_content.is_empty() {
        let preview = preview_text(&current_content.join(" "));

        let children = markdown_section_children(section_num, &current_section, &current_content);
        structure.push(DocNode {
            id: format!("section[{}]", section_num),
            tag: "_section".to_string(),
            attrs: vec![Attr {
                name: "heading".to_string(),
                value: current_section,
            }],
            text: Some(preview),
            children,
        });

        block_count += 1;
    }

    // If no sections found, create a single block
    if structure.is_empty() {
        let preview = preview_text(content);

        structure.push(DocNode {
            id: "document".to_string(),
            tag: "_document".to_string(),
            attrs: vec![],
            text: Some(preview),
            children: Vec::new(),
        });

        block_count = 1;
        total_tokens = estimate_tokens(content);
    }

    Ok(OptimizedScanResult {
        file_type: FileType::Markdown,
        structure,
        block_count,
        total_tokens,
    })
}

/// Build the hydrated children of a markdown section: the heading (if any) plus
/// every body line, each as its own text node, so grep/read reach the whole
/// section rather than only its truncated preview.
fn markdown_section_children(section_num: usize, heading: &str, body: &[String]) -> Vec<DocNode> {
    let mut children = Vec::new();
    if !heading.is_empty() {
        children.push(DocNode {
            id: format!("section[{section_num}].heading"),
            tag: "_md_heading".to_string(),
            attrs: Vec::new(),
            text: Some(heading.to_string()),
            children: Vec::new(),
        });
    }
    for (i, line) in body.iter().enumerate() {
        children.push(DocNode {
            id: format!("section[{section_num}].line[{i}]"),
            tag: "_md_line".to_string(),
            attrs: Vec::new(),
            text: Some(line.clone()),
            children: Vec::new(),
        });
    }
    children
}

/// Scan binary format (JPG, PNG) with metadata blocks.
/// Returns metadata without loading full image data.
pub fn scan_binary(path: &str, bytes: &[u8]) -> Result<OptimizedScanResult> {
    let file_type = FileType::Image;

    let mut structure = Vec::new();

    // Extract basic metadata
    let mut attrs = vec![Attr {
        name: "size".to_string(),
        value: format!("{} bytes", bytes.len()),
    }];

    // Try to detect image dimensions for PNG
    if path.ends_with(".png") && bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        // PNG signature
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        attrs.push(Attr {
            name: "width".to_string(),
            value: width.to_string(),
        });
        attrs.push(Attr {
            name: "height".to_string(),
            value: height.to_string(),
        });
    }

    // Try to detect image dimensions for JPEG
    if (path.ends_with(".jpg") || path.ends_with(".jpeg"))
        && bytes.len() >= 2
        && &bytes[0..2] == b"\xff\xd8"
    {
        {
            // JPEG signature - try to find SOF marker
            let mut i = 2;
            while i < bytes.len() - 9 {
                if bytes[i] == 0xff {
                    let marker = bytes[i + 1];
                    if marker == 0xc0 || marker == 0xc2 {
                        // SOF marker
                        let height = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
                        let width = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]);
                        attrs.push(Attr {
                            name: "width".to_string(),
                            value: width.to_string(),
                        });
                        attrs.push(Attr {
                            name: "height".to_string(),
                            value: height.to_string(),
                        });
                        break;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }

    let dims = match (
        attrs.iter().find(|a| a.name == "width").map(|a| &a.value),
        attrs.iter().find(|a| a.name == "height").map(|a| &a.value),
    ) {
        (Some(w), Some(h)) => format!("{w}x{h}, "),
        _ => String::new(),
    };
    structure.push(DocNode {
        id: "metadata".to_string(),
        tag: "_metadata".to_string(),
        attrs,
        text: Some(format!("Image ({dims}{} bytes)", bytes.len())),
        children: Vec::new(),
    });

    Ok(OptimizedScanResult {
        file_type,
        structure,
        block_count: 1,
        total_tokens: 10, // Minimal tokens for metadata
    })
}

/// Scan Mermaid (mmd) diagram with statement-level blocks.
/// Groups by diagram type and subgraphs.
pub fn scan_mermaid(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let content = std::str::from_utf8(bytes).with_context(|| "invalid UTF-8 in mermaid file")?;

    let mut structure = Vec::new();

    // First non-empty, non-comment line is the diagram type
    let diagram_type = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("diagram")
        .to_string();

    // Header block describing the diagram
    structure.push(DocNode {
        id: "diagram".to_string(),
        tag: "_mermaid".to_string(),
        attrs: vec![Attr {
            name: "type".to_string(),
            value: diagram_type.clone(),
        }],
        text: Some(format!("Mermaid {diagram_type} diagram")),
        children: Vec::new(),
    });

    // Group statements into subgraphs / top-level body
    let mut subgraph_idx = 0usize;
    let mut current: Vec<String> = Vec::new();
    let mut current_name: Option<String> = None;

    let flush =
        |structure: &mut Vec<DocNode>, idx: &mut usize, name: &Option<String>, lines: &[String]| {
            if lines.is_empty() {
                return;
            }
            let preview = preview_text(&lines.join(" "));
            let id = match name {
                Some(n) => format!("subgraph:{n}"),
                None => format!("body[{idx}]"),
            };
            // Carry every statement as a child so grep/read reach statements
            // past the preview cutoff.
            let children = lines
                .iter()
                .enumerate()
                .map(|(i, line)| DocNode {
                    id: format!("{id}.stmt[{i}]"),
                    tag: "_mermaid_stmt".to_string(),
                    attrs: Vec::new(),
                    text: Some(line.clone()),
                    children: Vec::new(),
                })
                .collect();
            structure.push(DocNode {
                id,
                tag: "_mermaid_group".to_string(),
                attrs: vec![Attr {
                    name: "statements".to_string(),
                    value: lines.len().to_string(),
                }],
                text: Some(preview),
                children,
            });
            *idx += 1;
        };

    let mut seen_header = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("%%") {
            continue;
        }
        if !seen_header {
            // Skip the diagram-type line itself
            seen_header = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("subgraph") {
            flush(&mut structure, &mut subgraph_idx, &current_name, &current);
            current.clear();
            current_name = Some(rest.trim().trim_matches('"').to_string());
        } else if trimmed == "end" {
            flush(&mut structure, &mut subgraph_idx, &current_name, &current);
            current.clear();
            current_name = None;
        } else {
            current.push(trimmed.to_string());
        }
    }
    flush(&mut structure, &mut subgraph_idx, &current_name, &current);

    let total_tokens = structure.iter().map(estimate_node_tokens).sum();
    let block_count = structure.len();

    Ok(OptimizedScanResult {
        file_type: FileType::Mermaid,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan CSV with a header block plus row-range blocks (100 rows each).
pub fn scan_csv(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let content = std::str::from_utf8(bytes).with_context(|| "invalid UTF-8 in csv file")?;

    let lines: Vec<&str> = content.lines().collect();
    let mut structure = Vec::new();

    if lines.is_empty() {
        return Ok(OptimizedScanResult {
            file_type: FileType::Csv,
            structure,
            block_count: 0,
            total_tokens: 0,
        });
    }

    // Header block (first line = column names)
    let header = lines[0];
    let columns: Vec<&str> = header.split(',').collect();
    structure.push(DocNode {
        id: "header".to_string(),
        tag: "_csv_header".to_string(),
        attrs: vec![Attr {
            name: "columns".to_string(),
            value: columns.len().to_string(),
        }],
        text: Some(format!("Columns: {}", preview_text(&columns.join(", ")))),
        children: Vec::new(),
    });

    // Row-range blocks over the data rows
    let data_rows = &lines[1..];
    const CHUNK: usize = 100;
    let mut start = 0usize;
    while start < data_rows.len() {
        let end = (start + CHUNK).min(data_rows.len());
        let preview = data_rows
            .get(start)
            .map(|r| preview_text(r))
            .unwrap_or_default();
        // Carry every row in the range as a child so grep/read reach the whole
        // chunk, not just its first row.
        let children = data_rows[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| DocNode {
                id: format!("rows[{}-{}].r[{}]", start, end - 1, start + i),
                tag: "_csv_row".to_string(),
                attrs: Vec::new(),
                text: Some(line.to_string()),
                children: Vec::new(),
            })
            .collect();
        structure.push(DocNode {
            id: format!("rows[{}-{}]", start, end - 1),
            tag: "_csv_rows".to_string(),
            attrs: vec![Attr {
                name: "rows".to_string(),
                value: (end - start).to_string(),
            }],
            text: Some(format!("Rows {}-{}: {}", start, end - 1, preview)),
            children,
        });
        start = end;
    }

    let total_tokens = structure.iter().map(estimate_node_tokens).sum();
    let block_count = structure.len();

    Ok(OptimizedScanResult {
        file_type: FileType::Csv,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan ZIP archive with one block per entry (name, size).
pub fn scan_zip(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).with_context(|| "failed to read zip archive")?;

    let mut structure = Vec::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("failed to read zip entry {i}"))?;
        let name = entry.name().to_string();
        let size = entry.size();
        let kind = if entry.is_dir() { "dir" } else { "file" };
        structure.push(DocNode {
            id: format!("entry:{name}"),
            tag: "_zip_entry".to_string(),
            attrs: vec![
                Attr {
                    name: "size".to_string(),
                    value: size.to_string(),
                },
                Attr {
                    name: "kind".to_string(),
                    value: kind.to_string(),
                },
            ],
            text: Some(format!("{name} ({size} bytes)")),
            children: Vec::new(),
        });
    }

    let total_tokens = structure.iter().map(estimate_node_tokens).sum();
    let block_count = structure.len();

    Ok(OptimizedScanResult {
        file_type: FileType::Archive,
        structure,
        block_count,
        total_tokens,
    })
}

/// Scan HTML into title / heading / paragraph blocks.
///
/// Shares the editing handler's tokenizer so every id `scan` advertises
/// (`title`, `section[N]`, `paragraph[N]`) is exactly what `edit` accepts.
pub fn scan_html(bytes: &[u8]) -> Result<OptimizedScanResult> {
    let content = std::str::from_utf8(bytes).with_context(|| "invalid UTF-8 in html file")?;

    let mut structure: Vec<DocNode> = super::html::tokenize(content)
        .into_iter()
        .map(|el| {
            let tag = match el.id.as_str() {
                "title" => "_html_title",
                id if id.starts_with("section") => "_html_section",
                _ => "_html_paragraph",
            };
            let full = strip_html(&el.text);
            // Keep the truncated preview as the block's own text, but carry the
            // full text as a child so grep/read reach content past the preview.
            let children = if full.trim().is_empty() {
                Vec::new()
            } else {
                vec![DocNode {
                    id: format!("{}.text", el.id),
                    tag: "_html_text".to_string(),
                    attrs: Vec::new(),
                    text: Some(full.clone()),
                    children: Vec::new(),
                }]
            };
            DocNode {
                id: el.id,
                tag: tag.to_string(),
                attrs: vec![Attr {
                    name: "level".to_string(),
                    value: el.tag,
                }],
                text: Some(preview_text(&full)),
                children,
            }
        })
        .collect();

    // If nothing addressable was found, emit a single document block.
    if structure.is_empty() {
        let text = strip_html(content);
        structure.push(DocNode {
            id: "document".to_string(),
            tag: "_html_document".to_string(),
            attrs: Vec::new(),
            text: Some(preview_text(&text)),
            children: Vec::new(),
        });
    }

    let total_tokens = structure.iter().map(estimate_node_tokens).sum();
    let block_count = structure.len();

    Ok(OptimizedScanResult {
        file_type: FileType::Html,
        structure,
        block_count,
        total_tokens,
    })
}

// ── Helper functions ──────────────────────────────────────────────────────

/// Truncate text to a short preview (<= 100 chars).
fn preview_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 100 {
        let truncated: String = collapsed.chars().take(97).collect();
        format!("{truncated}...")
    } else {
        collapsed
    }
}

/// Strip HTML tags, returning text content.
fn strip_html(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Count nodes and tokens recursively.
fn count_nodes(nodes: &[DocNode], count: &mut usize, tokens: &mut usize) {
    for node in nodes {
        *count += 1;
        *tokens += estimate_node_tokens(node);
        count_nodes(&node.children, count, tokens);
    }
}

/// Estimate tokens for a node.
fn estimate_node_tokens(node: &DocNode) -> usize {
    let text_len = node.text.as_ref().map(|t| t.len()).unwrap_or(0);
    let attrs_len: usize = node
        .attrs
        .iter()
        .map(|a| a.name.len() + a.value.len())
        .sum();
    (text_len + attrs_len) / 4 + 1
}

/// Estimate tokens for text.
fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let word_count = text.split_whitespace().count();
    (word_count as f64 * 1.3) as usize + 1
}

/// Categorize SVG element by tag name.
fn categorize_svg_element(tag: &str) -> String {
    match tag {
        "path" | "line" | "rect" | "circle" | "ellipse" | "polygon" | "polyline" => {
            "shapes".to_string()
        }
        "text" | "tspan" | "textPath" => "text".to_string(),
        "g" | "svg" => "groups".to_string(),
        "defs" | "style" | "metadata" => "definitions".to_string(),
        "image" => "images".to_string(),
        _ => "other".to_string(),
    }
}

// ── Zip / OOXML helpers ───────────────────────────────────────────────────

/// List entry names in a zip container.
fn zip_entry_names(container: &[u8]) -> Result<Vec<String>> {
    let zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    Ok(zip.file_names().map(|s| s.to_string()).collect())
}

/// Read one entry's decompressed bytes from a zip container.
fn zip_read_part(container: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut zip = ZipArchive::new(Cursor::new(container))
        .with_context(|| "opening OOXML container (not a valid zip?)")?;
    let mut f = zip
        .by_name(name)
        .with_context(|| format!("part `{name}` not found in container"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Numeric index embedded in a slide part name (`ppt/slides/slide12.xml` -> 12).
fn slide_number(part: &str) -> usize {
    part.rsplit('/')
        .next()
        .and_then(|f| f.strip_prefix("slide"))
        .and_then(|f| f.strip_suffix(".xml"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Count `<a:t>` text runs and collect the first runs' text for a preview,
/// without a full XML parse. Handles runs carrying attributes
/// (`<a:t xml:space="preserve">`) and unescapes XML entities.
fn scan_drawingml_text(content: &str) -> (usize, String) {
    let mut count = 0usize;
    let mut preview = String::new();
    for_each_drawingml_run(content, |text| {
        count += 1;
        if preview.chars().count() < 100 {
            if !preview.is_empty() {
                preview.push(' ');
            }
            preview.push_str(text);
        }
    });
    (count, preview)
}

/// Split a slide's DrawingML into paragraph text, one `String` per `<a:p>`,
/// with each paragraph's `<a:t>` runs concatenated and entity-unescaped. Empty
/// paragraphs are dropped. Slides with no `<a:p>` fall back to a single
/// whole-part paragraph so stray runs are still surfaced.
fn slide_paragraphs(content: &str) -> Vec<String> {
    let mut paras = Vec::new();
    for chunk in content.split("<a:p>").skip(1) {
        let body = chunk.split("</a:p>").next().unwrap_or(chunk);
        let text = drawingml_runs_text(body);
        if !text.trim().is_empty() {
            paras.push(text);
        }
    }
    if paras.is_empty() {
        let text = drawingml_runs_text(content);
        if !text.trim().is_empty() {
            paras.push(text);
        }
    }
    paras
}

/// Concatenate the text of every `<a:t>` run in `fragment` (space-separated),
/// unescaping XML entities.
fn drawingml_runs_text(fragment: &str) -> String {
    let mut out = String::new();
    for_each_drawingml_run(fragment, |text| {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
    });
    out
}

/// Invoke `f` with the unescaped text of each `<a:t …>…</a:t>` run in `content`,
/// in document order. Tolerates run attributes and self-closing `<a:t/>`.
fn for_each_drawingml_run(content: &str, mut f: impl FnMut(&str)) {
    let mut rest = content;
    while let Some(open) = rest.find("<a:t") {
        let after = &rest[open + 4..];
        // A run element opens with `>`, a space (attrs), or `/` (self-close);
        // anything else (e.g. a hypothetical `<a:table`) is a false positive.
        if !(after.starts_with('>') || after.starts_with(' ') || after.starts_with('/')) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..]; // self-closing run, no text
            continue;
        }
        let body = &after[gt + 1..];
        let Some(close) = body.find("</a:t>") else { break };
        f(&super::xml_unescape(&body[..close]));
        rest = &body[close + "</a:t>".len()..];
    }
}
