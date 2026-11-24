//! Semantic document text extraction for search and `--text` output.
//!
//! Bead: `glisk-2hp.6` · spec: `FEASIBILITY.md#navigation-and-search` ·
//! contract: one local, self-contained document from [`crate::document`].
//!
//! The walker reads the live Blitz DOM, never layout, fonts, Kitty state, or a
//! terminal. Each semantic block keeps its [`NodeId`] so a later search can
//! reveal the block and its enclosing disclosures. Inline descendants are
//! concatenated before whitespace is normalized, so a match can span styling
//! boundaries such as `<strong>` or `<span>`.
//!
//! The same block list renders readable plain text and the normalized text a
//! search matches against. Structural order follows the DOM, not CSS layout.
//!
//! Integration note: the wiring beads add `mod text;` to `src/main.rs`
//! and call [`extract`] on the ingested document. This module has no terminal
//! dependency, so it can be tested without Kitty, a graphics terminal, or a
//! chosen font.

use blitz_dom::{BaseDocument, Node, NodeData, NodeId};

/// Spaces substituted for each tab in preformatted text.
pub const CODE_TAB_WIDTH: usize = 4;
/// Two spaces per nested list level in plain text.
pub const LIST_INDENT: usize = 2;

/// The semantic role of one extracted block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// HTML heading; the value is the level, 1 through 6.
    Heading(u8),
    /// Ordinary paragraph or stray inline run.
    Paragraph,
    /// One list item. `TextBlock::depth` is the nesting level.
    ListItem,
    /// Definition-list term.
    DefinitionTerm,
    /// Definition-list description.
    Definition,
    /// Preformatted text; the block text keeps internal line breaks.
    Code,
    /// Table caption.
    TableCaption,
    /// One table row. Cells are joined with `" | "` in source order.
    TableRow,
    /// Image description from `alt`, `aria-label`, or `title`.
    Image,
    /// Inline SVG title/description summary.
    Chart,
    /// `<summary>` text of a disclosure.
    Summary,
    /// `<figcaption>` text.
    Caption,
}

/// One searchable document block tied to a stable live DOM node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBlock {
    /// Stable node id for the block's anchored element.
    pub node: NodeId,
    /// Semantic role in the document.
    pub kind: BlockKind,
    /// List, definition, or nested structural depth. Zero for top-level blocks.
    pub depth: usize,
    /// Display text for plain output. Code blocks may contain `\n`.
    pub text: String,
    /// Whitespace-normalized text used for case-insensitive search. It never
    /// contains document-supplied control characters.
    pub search_text: String,
}

impl TextBlock {
    /// Normalized text a search should match. Inline styling boundaries are
    /// already removed.
    pub fn searchable_text(&self) -> &str {
        &self.search_text
    }

    /// Case-insensitive substring match against this block's normalized text.
    /// An empty query never matches.
    pub fn matches(&self, query: &str) -> bool {
        let needle = normalize_search_query(query);
        !needle.is_empty() && self.search_text.to_lowercase().contains(&needle)
    }
}

/// Extraction result for one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentText {
    blocks: Vec<TextBlock>,
    plain_text: String,
}

impl DocumentText {
    /// Blocks in document order.
    pub fn blocks(&self) -> &[TextBlock] {
        &self.blocks
    }

    /// Rendered plain UTF-8 text, ending with one newline unless empty.
    pub fn plain_text(&self) -> &str {
        &self.plain_text
    }

    /// Whether the document produced no readable text.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Blocks whose normalized text matches `query`, in document order.
    pub fn matching_blocks<'a>(
        &'a self,
        query: &'a str,
    ) -> impl Iterator<Item = &'a TextBlock> + 'a {
        let needle = normalize_search_query(query);
        self.blocks.iter().filter(move |block| {
            !needle.is_empty() && block.search_text.to_lowercase().contains(&needle)
        })
    }
}

/// Walk a resolved document and extract searchable blocks plus plain text.
///
/// The caller owns any [`crate::document`] resource-policy checks. Extraction
/// only reads the in-memory DOM and requires no graphics capability or font.
pub fn extract(document: &BaseDocument) -> DocumentText {
    let mut walker = Walker {
        document,
        blocks: Vec::new(),
    };
    walker.walk(document.root_node().id, 0);
    let plain_text = render_plain_text(&walker.blocks);
    DocumentText {
        blocks: walker.blocks,
        plain_text,
    }
}

/// Convenience wrapper for callers that only need the `--text` payload.
pub fn extract_plain_text(document: &BaseDocument) -> String {
    extract(document).plain_text
}

/// Render blocks as terminal-safe plain UTF-8 text.
///
/// Newlines in the result are app-owned block separators; the only other
/// embedded line breaks come from preformatted code, where tabs were expanded.
/// All blocks are already stripped of ESC/C0/DEL/C1 controls.
pub fn render_plain_text(blocks: &[TextBlock]) -> String {
    let mut out = String::new();
    let mut previous = None;

    for block in blocks.iter().filter(|block| !block.text.is_empty()) {
        if previous.is_some() && !continues_group(previous, Some(block.kind)) {
            out.push('\n');
        }
        match block.kind {
            BlockKind::Heading(level) => {
                let level = level.clamp(1, 6) as usize;
                out.push_str(&"#".repeat(level));
                out.push(' ');
                out.push_str(&block.text);
                out.push('\n');
            }
            BlockKind::ListItem => {
                out.push_str(&" ".repeat(block.depth.saturating_sub(1) * LIST_INDENT));
                out.push_str("- ");
                out.push_str(&block.text);
                out.push('\n');
            }
            BlockKind::Definition => {
                out.push_str(&" ".repeat(block.depth.max(1) * LIST_INDENT));
                out.push_str(": ");
                out.push_str(&block.text);
                out.push('\n');
            }
            BlockKind::Code => {
                for line in block.text.split('\n') {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            BlockKind::TableRow => {
                out.push_str("| ");
                out.push_str(&block.text);
                out.push_str(" |\n");
            }
            BlockKind::Image => {
                out.push_str("[image: ");
                out.push_str(&block.text);
                out.push_str("]\n");
            }
            BlockKind::Chart => {
                out.push_str("Chart: ");
                out.push_str(&block.text);
                out.push('\n');
            }
            BlockKind::Paragraph
            | BlockKind::DefinitionTerm
            | BlockKind::TableCaption
            | BlockKind::Summary
            | BlockKind::Caption => {
                out.push_str(&block.text);
                out.push('\n');
            }
        }
        previous = Some(block.kind);
    }

    let trimmed = out.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn continues_group(previous: Option<BlockKind>, current: Option<BlockKind>) -> bool {
    match (previous, current) {
        (Some(BlockKind::ListItem), Some(BlockKind::ListItem)) => true,
        (Some(BlockKind::TableCaption | BlockKind::TableRow), Some(BlockKind::TableRow)) => true,
        _ => false,
    }
}

struct Walker<'a> {
    document: &'a BaseDocument,
    blocks: Vec<TextBlock>,
}

impl Walker<'_> {
    fn walk(&mut self, id: NodeId, list_depth: usize) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        if self.is_hidden(node) {
            return;
        }
        if matches!(node.data, NodeData::Document(_)) {
            for &child in &node.children {
                self.walk(child, list_depth);
            }
            return;
        }
        let Some(element) = node.element_data() else {
            return;
        };
        let tag = element.name.local.as_ref();

        match tag {
            "head" | "script" | "style" | "template" | "title" | "meta" | "link" | "base"
            | "iframe" | "object" | "embed" | "canvas" | "param" | "source" | "track" => {}
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag.as_bytes()[1] - b'0';
                self.push_inline(id, BlockKind::Heading(level), 0);
            }
            "p" => self.push_inline(id, BlockKind::Paragraph, list_depth),
            "pre" => self.push_code(id),
            "li" => self.walk_list_item(id, list_depth),
            "dt" => self.push_inline(id, BlockKind::DefinitionTerm, list_depth),
            "dd" => self.push_inline(id, BlockKind::Definition, list_depth + 1),
            "summary" => self.push_inline(id, BlockKind::Summary, list_depth),
            "figcaption" => self.push_inline(id, BlockKind::Caption, list_depth),
            "img" => self.push_image(id, list_depth),
            "svg" => self.push_chart(id, list_depth),
            "table" => self.walk_table(id, list_depth),
            "details" => self.walk_mixed_children(node, list_depth),
            "ul" | "ol" | "menu" => {
                for &child in &node.children {
                    self.walk(child, list_depth);
                }
            }
            "dl" => {
                for &child in &node.children {
                    self.walk(child, list_depth);
                }
            }
            _ => self.walk_mixed_children(node, list_depth),
        }
    }

    /// Walk a container whose children mix inline content and semantic blocks.
    /// Inline runs become paragraphs anchored to the container; recognized
    /// block children keep their own nodes and order.
    fn walk_mixed_children(&mut self, node: &Node, list_depth: usize) {
        let mut inline = String::new();
        for &child_id in &node.children {
            let Some(child) = self.document.get_node(child_id) else {
                continue;
            };
            if self.is_hidden(child) {
                continue;
            }
            if self.is_block_boundary(child) {
                self.flush_inline(node.id, BlockKind::Paragraph, list_depth, &mut inline);
                self.walk(child_id, list_depth);
            } else {
                self.collect_inline(child_id, &mut inline, &[]);
            }
        }
        self.flush_inline(node.id, BlockKind::Paragraph, list_depth, &mut inline);
    }

    fn walk_list_item(&mut self, id: NodeId, list_depth: usize) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        const EXCLUDED_BLOCKS: &[&str] = &["ul", "ol", "menu", "table", "pre", "details", "svg"];
        let mut inline = String::new();
        for &child in &node.children {
            self.collect_inline(child, &mut inline, EXCLUDED_BLOCKS);
        }
        self.flush_inline(id, BlockKind::ListItem, list_depth + 1, &mut inline);

        for &child in &node.children {
            let Some(child_node) = self.document.get_node(child) else {
                continue;
            };
            if self.is_hidden(child_node) {
                continue;
            }
            let tag = self.tag_of(child);
            if matches!(tag, Some("ul" | "ol" | "menu")) {
                self.walk(child, list_depth + 1);
            } else if matches!(tag, Some("table" | "pre" | "details" | "svg")) {
                self.walk(child, list_depth + 1);
            }
        }
    }

    fn walk_table(&mut self, id: NodeId, list_depth: usize) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };

        for &child in &node.children {
            if self.tag_of(child) == Some("caption") {
                self.push_inline(child, BlockKind::TableCaption, list_depth);
            }
        }

        let mut rows = Vec::new();
        self.collect_rows(id, &mut rows);
        for row in rows {
            let Some(row_node) = self.document.get_node(row) else {
                continue;
            };
            let cells: Vec<String> = row_node
                .children
                .iter()
                .filter(|&&cell| matches!(self.tag_of(cell), Some("th" | "td")))
                .map(|&cell| self.inline_text(cell, &[]))
                .collect();
            if cells.is_empty() {
                continue;
            }
            let text = cells.join(" | ");
            if let Some(block) = self.block(row, BlockKind::TableRow, list_depth, text) {
                self.blocks.push(block);
            }
        }
    }

    /// Collect table rows in DOM order without descending into a nested table.
    fn collect_rows(&self, id: NodeId, rows: &mut Vec<NodeId>) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        for &child in &node.children {
            let Some(child_node) = self.document.get_node(child) else {
                continue;
            };
            if self.is_hidden(child_node) {
                continue;
            }
            match self.tag_of(child) {
                Some("tr") => rows.push(child),
                Some("table") => {}
                _ => self.collect_rows(child, rows),
            }
        }
    }

    fn push_inline(&mut self, id: NodeId, kind: BlockKind, depth: usize) {
        let text = self.inline_text(id, &[]);
        if let Some(block) = self.block(id, kind, depth, text) {
            self.blocks.push(block);
        }
    }

    fn push_code(&mut self, id: NodeId) {
        let text = self.raw_pre_text(id);
        if let Some(block) = self.block(id, BlockKind::Code, 0, text) {
            self.blocks.push(block);
        }
    }

    fn push_image(&mut self, id: NodeId, depth: usize) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        let Some(text) = self.image_text(node) else {
            return;
        };
        if let Some(block) = self.block(id, BlockKind::Image, depth, text) {
            self.blocks.push(block);
        }
    }

    fn push_chart(&mut self, id: NodeId, depth: usize) {
        let text = self.chart_text(id);
        if let Some(block) = self.block(id, BlockKind::Chart, depth, text) {
            self.blocks.push(block);
        }
    }

    fn block(&self, id: NodeId, kind: BlockKind, depth: usize, text: String) -> Option<TextBlock> {
        if text.is_empty() {
            return None;
        }
        Some(TextBlock {
            node: id,
            kind,
            depth,
            search_text: normalize_search_text(&text),
            text,
        })
    }

    fn flush_inline(&mut self, anchor: NodeId, kind: BlockKind, depth: usize, inline: &mut String) {
        let text = sanitize_inline(inline);
        inline.clear();
        if let Some(block) = self.block(anchor, kind, depth, text) {
            self.blocks.push(block);
        }
    }

    fn inline_text(&self, id: NodeId, excluded: &[&str]) -> String {
        let mut raw = String::new();
        self.collect_inline(id, &mut raw, excluded);
        sanitize_inline(&raw)
    }

    /// Concatenate inline descendants before whitespace normalization. Blocks
    /// in `excluded` are skipped so a list item can keep its nested lists as
    /// separate blocks.
    fn collect_inline(&self, id: NodeId, out: &mut String, excluded: &[&str]) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        if self.is_hidden(node) {
            return;
        }
        match &node.data {
            NodeData::Text(text) => out.push_str(&strip_inline_controls(&text.content)),
            NodeData::Element(_) | NodeData::AnonymousBlock(_) => {
                let Some(element) = node.element_data() else {
                    return;
                };
                let tag = element.name.local.as_ref();
                if is_non_content(tag) || excluded.contains(&tag) {
                    return;
                }
                match tag {
                    "br" => out.push(' '),
                    "img" => {
                        if let Some(text) = self.image_text(node) {
                            out.push(' ');
                            out.push_str(&text);
                            out.push(' ');
                        }
                    }
                    "svg" => {
                        let text = self.chart_text(id);
                        if !text.is_empty() {
                            out.push(' ');
                            out.push_str(&text);
                            out.push(' ');
                        }
                    }
                    _ => {
                        for &child in &node.children {
                            self.collect_inline(child, out, excluded);
                        }
                    }
                }
            }
            NodeData::Document(_) | NodeData::Comment { .. } => {
                for &child in &node.children {
                    self.collect_inline(child, out, excluded);
                }
            }
        }
    }

    /// Literal text of a `<pre>` with CRLF normalized, tabs expanded, controls
    /// removed, and outer blank lines trimmed. Internal indentation survives.
    fn raw_pre_text(&self, id: NodeId) -> String {
        let mut raw = String::new();
        self.collect_pre(id, &mut raw);
        sanitize_pre(&raw)
    }

    fn collect_pre(&self, id: NodeId, out: &mut String) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        if self.is_hidden(node) {
            return;
        }
        match &node.data {
            NodeData::Text(text) => out.push_str(&text.content),
            NodeData::Element(_) | NodeData::AnonymousBlock(_) => {
                let Some(element) = node.element_data() else {
                    return;
                };
                let tag = element.name.local.as_ref();
                if is_non_content(tag) {
                    return;
                }
                match tag {
                    "br" => out.push('\n'),
                    "img" => {
                        if let Some(text) = self.image_text(node) {
                            out.push_str(&text);
                        }
                    }
                    "svg" => {}
                    _ => {
                        for &child in &node.children {
                            self.collect_pre(child, out);
                        }
                    }
                }
            }
            NodeData::Document(_) | NodeData::Comment { .. } => {
                for &child in &node.children {
                    self.collect_pre(child, out);
                }
            }
        }
    }

    fn image_text(&self, node: &Node) -> Option<String> {
        for name in ["alt", "aria-label", "title"] {
            if let Some(value) = attr(node, name) {
                let text = sanitize_inline(value);
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Inline SVG has no reliable drawn-text fallback; its authored title and
    /// description are the readable summary. Shape labels are deliberately not
    /// treated as prose.
    fn chart_text(&self, id: NodeId) -> String {
        let Some(node) = self.document.get_node(id) else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let Some(label) = attr(node, "aria-label") {
            parts.push(sanitize_inline(label));
        }
        for tag in ["title", "desc"] {
            if let Some(descendant) = self.first_descendant_with_tag(id, tag) {
                if let Some(node) = self.document.get_node(descendant) {
                    if let Some(text) = node.text_data() {
                        parts.push(sanitize_inline(&text.content));
                    } else {
                        parts.push(sanitize_inline(&node.text_content()));
                    }
                }
            }
        }
        parts.retain(|part| !part.is_empty());
        parts.dedup();
        if parts.is_empty() {
            let mut labels = Vec::new();
            self.collect_svg_text(id, &mut labels);
            parts.extend(labels.into_iter().map(|text| sanitize_inline(&text)));
        }
        parts.join(". ")
    }

    fn collect_svg_text(&self, id: NodeId, out: &mut Vec<String>) {
        let Some(node) = self.document.get_node(id) else {
            return;
        };
        for &child in &node.children {
            let Some(child_node) = self.document.get_node(child) else {
                continue;
            };
            match self.tag_of(child) {
                Some("text") => {
                    let text = sanitize_inline(&child_node.text_content());
                    if !text.is_empty() {
                        out.push(text);
                    }
                }
                Some("title" | "desc") => {}
                _ => self.collect_svg_text(child, out),
            }
        }
    }

    fn first_descendant_with_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        let node = self.document.get_node(id)?;
        for &child in &node.children {
            if self.tag_of(child) == Some(tag) {
                return Some(child);
            }
            if let Some(found) = self.first_descendant_with_tag(child, tag) {
                return Some(found);
            }
        }
        None
    }

    fn tag_of(&self, id: NodeId) -> Option<&str> {
        self.document
            .get_node(id)
            .and_then(Node::element_data)
            .map(|element| element.name.local.as_ref())
    }

    fn is_block_boundary(&self, node: &Node) -> bool {
        let Some(element) = node.element_data() else {
            return false;
        };
        is_block_tag(element.name.local.as_ref())
    }

    /// Skip non-content elements and content the author explicitly hid. The
    /// computed-style checks use the resolved Blitz tree and stay independent
    /// of fonts, layout geometry, and the terminal.
    fn is_hidden(&self, node: &Node) -> bool {
        if node.data.has_attr(blitz_dom::local_name!("hidden")) {
            return true;
        }
        if attr(node, "aria-hidden").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
            return true;
        }
        if let Some(style) = attr(node, "style") {
            if style_hides(style) {
                return true;
            }
        }
        // A closed <details> hides its body only for layout. Search and text
        // extraction must still include that body, so computed layout hiding is
        // ignored for descendants of a closed disclosure. Explicit `hidden`,
        // `aria-hidden`, and inline style hiding still apply above.
        if !self.inside_closed_details(node) {
            if let Some(styles) = node.primary_styles() {
                let visibility = format!("{:?}", styles.clone_visibility());
                if visibility == "Hidden" || visibility == "Collapse" {
                    return true;
                }
            }
            // `Node::taffy_display` is the public bridge from computed style to
            // a display value. Debug formatting avoids adding a direct
            // taffy/style dependency just to compare one enum variant.
            if format!("{:?}", node.taffy_display()) == "None" {
                return true;
            }
        }
        false
    }

    fn inside_closed_details(&self, node: &Node) -> bool {
        let mut parent = node.parent;
        while let Some(id) = parent {
            let Some(ancestor) = self.document.get_node(id) else {
                break;
            };
            if self.tag_of(id) == Some("details")
                && !ancestor.data.has_attr(blitz_dom::local_name!("open"))
            {
                return true;
            }
            parent = ancestor.parent;
        }
        false
    }
}

fn attr<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.attrs()?.iter().find_map(|attribute| {
        let local: &str = attribute.name.local.as_ref();
        (local == name).then_some(attribute.value.as_str())
    })
}

fn style_hides(style: &str) -> bool {
    let compact: String = style
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact.split(';').any(|declaration| {
        declaration.starts_with("display:none")
            || declaration.starts_with("visibility:hidden")
            || declaration.starts_with("visibility:collapse")
    })
}

fn is_non_content(tag: &str) -> bool {
    matches!(
        tag,
        "script"
            | "style"
            | "template"
            | "head"
            | "title"
            | "meta"
            | "link"
            | "base"
            | "iframe"
            | "object"
            | "embed"
            | "canvas"
            | "param"
            | "source"
            | "track"
    )
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "caption"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "img"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "svg"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Remove escape and control characters while preserving ordinary whitespace
/// for later inline normalization.
fn strip_inline_controls(input: &str) -> String {
    input
        .chars()
        .filter_map(|ch| {
            if ch == '\r' || ch == '\n' || ch == '\t' {
                Some(' ')
            } else if is_forbidden_control(ch) {
                None
            } else {
                Some(ch)
            }
        })
        .collect()
}

fn sanitize_inline(input: &str) -> String {
    strip_inline_controls(input)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalize preformatted text for display. Tabs become app-owned spaces so a
/// tab cannot move the reader's cursor; only app-owned `\n` survives.
fn sanitize_pre(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    for ch in normalized.chars() {
        match ch {
            '\n' => out.push('\n'),
            '\t' => out.push_str(&" ".repeat(CODE_TAB_WIDTH)),
            ch if is_forbidden_control(ch) => {}
            ch => out.push(ch),
        }
    }
    out.trim_matches('\n').to_string()
}

fn is_forbidden_control(ch: char) -> bool {
    matches!(ch, '\u{1b}' | '\u{7f}')
        || ('\u{80}'..='\u{9f}').contains(&ch)
        || (ch <= '\u{1f}' && ch != '\n' && ch != '\r' && ch != '\t')
}

fn normalize_search_text(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_search_query(input: &str) -> String {
    normalize_search_text(&sanitize_inline(input)).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_controls_are_removed_but_spaces_survive() {
        let text = format!("a{}b{}\tc\n", '\u{1b}', '\u{7f}');
        assert_eq!(sanitize_inline(&text), "ab c");
        assert_eq!(sanitize_pre(&text), "ab\tc".replace('\t', "    "));
    }

    #[test]
    fn pre_tabs_expand_and_line_breaks_survive() {
        let text = "if ok {\r\n\trun();\r\n}\r\n";
        assert_eq!(sanitize_pre(text), "if ok {\n    run();\n}");
    }
}
