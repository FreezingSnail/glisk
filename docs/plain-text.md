# Searchable text and plain output

Bead: `glisk-2hp.6` · module: `spike/src/text.rs` · tests:
`spike/tests/text_output.rs` · spec: `FEASIBILITY.md#navigation-and-search`

This module is the one semantic walk used for both in-viewer search and
`glisk report.html --text`. It reads the live Blitz DOM after the bounded
ingestion path in `spike/src/document.rs` has applied the local-only resource
policy. It does not depend on Kitty, terminal capabilities, fonts, paint, or a
viewport.

The module is wired into `spike/src/main.rs` by bead `glisk-2hp.8`: both
explicit `--text` and automatic plain-text fallback call
`text::extract_plain_text` after bounded ingestion. The integration test still
includes the module with `#[path = "../src/text.rs"]`.

## API

| Item | Contract |
|---|---|
| `extract(&BaseDocument) -> DocumentText` | Walk the document once. Each block keeps its live `NodeId`; the result also carries rendered plain text. |
| `extract_plain_text(&BaseDocument) -> String` | Convenience wrapper for the `--text` payload. |
| `render_plain_text(&[TextBlock]) -> String` | Render an existing block list with app-owned breaks and indentation. |
| `DocumentText::blocks()` | Blocks in document order. |
| `DocumentText::plain_text()` | Plain UTF-8 text ending in one newline, or an empty string. |
| `DocumentText::matching_blocks(query)` | Case-insensitive matching blocks, preserving order. |
| `DocumentText::is_empty()` | True when the document produced no readable text. |
| `TextBlock::searchable_text()` / `matches(query)` | Normalized block text and a case-insensitive substring check. |

`TextBlock` exposes `node`, `kind`, `depth`, `text`, and `search_text`.
`text` is display text; `search_text` collapses all whitespace, including the
line breaks and indentation of code blocks, so a query can match across a code
line boundary. Inline styling boundaries are already removed from both.

A later search implementation can walk `blocks()`, call `TextBlock::matches`,
and use `TextBlock::node` to scroll the block into view. When the matched block
is inside a closed `<details>`, the search code must expand the enclosing
disclosure before scrolling. Extraction deliberately includes those bodies
regardless of open state; it does not mutate disclosure state.

## Semantic walk

The walker follows DOM order. Recognized element kinds become blocks:

| HTML | Block kind | Notes |
|---|---|---|
| `h1`–`h6` | `Heading(level)` | Rendered with `#` markers in plain text. |
| `p`, stray inline runs in containers | `Paragraph` | Inline descendants are concatenated before whitespace normalization. |
| `li` | `ListItem` | Depth is 1 for a top-level item; nested lists indent two spaces per level. |
| `dt` / `dd` | `DefinitionTerm` / `Definition` | Simple definition lists. |
| `pre` | `Code` | Internal line breaks and indentation are kept; tabs become four spaces. |
| `table` | `TableCaption` + `TableRow` | Caption first, then rows and header cells in DOM order; cells join with `" \| "`. |
| `img` | `Image` or inline text | `alt`, then `aria-label`, then `title`; inline images join the surrounding paragraph. |
| inline `svg` | `Chart` | Authored `title`, `desc`, and `aria-label` become the summary. |
| `details` / `summary` | `Summary` + body blocks | Bodies are included whether or not the disclosure is open. |
| `figcaption` | `Caption` | Caption text stays near its figure content. |

Non-content elements are skipped: `head`, `script`, `style`, `template`,
`title`, `meta`, `link`, `base`, `iframe`, `object`, `embed`, `canvas`,
`param`, `source`, and `track`. Explicitly hidden content is skipped through
the `hidden` attribute, `aria-hidden="true"`, and inline or computed
`display:none` / `visibility:hidden|collapse`. The computed-style check is
bypassed for descendants of a closed `<details>` because the browser hides
that body only for layout; the text contract requires it to remain readable.

Whitespace is normalized per inline block: source whitespace collapses to one
space, and separate inline elements are joined without inventing a space. Thus
`<span>bound</span><span>aries</span>` becomes `boundaries`, while
`<strong>across</strong> <em>styled</em>` keeps its authored space.

## Plain-text formatting

- Headings use `#`, `##`, and up to six leading markers.
- Paragraphs and most blocks are separated by one blank line.
- Top-level list items start with `- `; nested levels add two spaces.
- Table rows render as `| cell | cell |`; caption and rows stay contiguous.
- Code keeps its own line breaks and indentation; tabs are expanded to four
  spaces. A code block is separated from surrounding blocks by blank lines.
- Images render as `[image: description]`; SVG summaries render as
  `Chart: title. description`.
- Definition descriptions use a `:` marker with list-style indentation.
- Links keep their visible label; the output has no URL rewriting.

The final string is trimmed and ends with exactly one newline unless no block
was readable.

## Control sanitization

Every path that copies document text — headings, paragraph runs, table cells,
captions, image text, SVG title/description, summaries, and code — passes
through the same sanitizer:

- ESC (`U+001B`), DEL (`U+007F`), C1 (`U+0080..=U+009F`), and C0 controls other
  than tab, CR, and LF are removed.
- CR and CRLF become LF in preformatted text; tabs become spaces.
- Inline text reduces remaining whitespace to single spaces, so no raw control
  reaches the terminal.
- The only control character in rendered output is the app-owned LF that
  separates blocks or code lines.

The search text is normalized from the already-sanitized block text, so a search
cannot reintroduce escape sequences.

## Tests

```
cd spike
cargo test --test text_output
```

Coverage:

- the reference report's headings, card prose, headered table order, SVG chart
  title/description, caption, summary list, and both closed evidence bodies;
- searchable text split across `<strong>`, `<em>`, and adjacent `<span>`s,
  including stable block node ids;
- skipping script, style, template, head/title, `hidden`, `aria-hidden`,
  inline display/visibility hiding, and CSS class display hiding;
- closed and open disclosure bodies in document order;
- nested list indentation, preformatted code indentation, and row/header order;
- inline image alt text, standalone image descriptions, and captions;
- ESC, BEL, DEL, and C1 removal from paragraphs, code, and image text;
- deterministic re-extraction and reuse of the same plain output.

`text.rs` also has unit tests for control stripping and tab expansion.

## Limitations

- This is a semantic text view, not a layout replica. CSS reordering, flex/grid
  placement, `order`, and visual columns are not simulated; DOM order wins.
- Tables are flattened to one text row per `<tr>`. Column spans, row spans, and
  per-cell block nesting are not modeled beyond joining the cell's inline text.
- Nested tables are deliberately not walked as separate row sets.
- A list item's ordinary inline content and nested lists/pre blocks are split,
  but other nested block structure inside an `<li>` is folded into the item's
  text.
- SVG extraction reads authored `title`/`desc`/`aria-label`. If none exists it
  falls back to descendant `<text>` labels; shape geometry and color encoding
  are not interpreted.
- Hidden detection is based on resolved Blitz styles for the default viewport
  used during ingestion. A later viewport-dependent author style is not
  re-evaluated by extraction.
- `aria-hidden="true"` is treated as hidden for text output. This is a
  deliberate text-view choice; it is not a general accessibility audit.
- Link targets, form values, canvas paint, media elements, and subdocuments are
  not turned into text. The local-only ingestion policy already denies external
  resources and strips `srcdoc`.
- Plain output is UTF-8 with app-owned ASCII markers and newlines. It assumes a
  terminal or pipe that accepts UTF-8, but it never emits terminal controls.
