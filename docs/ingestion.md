# Bounded document ingestion

Bead: `glisk-2hp.2` · policy source: `FEASIBILITY.md#offline-authoring-and-resource-limits` · code: `spike/src/document.rs`

This module is the single entry point for document bytes. Glisk displays one
local `.htm`/`.html` file (or the same bytes from stdin); it is not a web
browser. File input, stdin input, and the later text and graphics output paths
must all pass through this module, so one resource policy governs every
document before it reaches the Blitz pipeline. Nothing here fetches external
URLs or provides general URL browsing.

## Entry points

| Function | Use |
|---|---|
| `ingest_file(path, viewport, limits)` | File path source. |
| `ingest_stdin(viewport, limits)` | `-` source, read through the same bounded reader. |
| `ingest_reader(reader, viewport, limits)` | Any byte source, including tests. |
| `read_html_file(path, limits)` / `read_bounded(reader, limit)` | Read + UTF-8 validation without parsing. |
| `open_document(html, viewport, limits)` | Parse, budget-check, sanitize, and resolve in-memory HTML. |

`ingest_*` returns `IngestedDocument { document, policy, stats }`. The
`BaseDocument` is already parsed and resolved with a `Viewport`; `stats` records
the node count and depth that were checked before layout. `policy` retains
denials and admitted-image counters for diagnostics and tests.

Input failures are `IngestError`: `TooLarge { limit, observed }`,
`InvalidUtf8 { valid_up_to }`, `Read`, `NodeBudget`, `DepthBudget`. They map to
exit status 1 in the CLI, with the message written to stderr before the terminal
is switched to the alternate screen.

## Budgets

Constants match the provisional v0 table. Tests can pass a smaller `Limits`
value for boundary checks; production uses `Limits::default()`.

| Budget | Constant | Enforcement |
|---|---:|---|
| HTML input | `MAX_HTML_BYTES` (4 MiB) | `read_bounded` reads at most `limit + 1`, then validates UTF-8. |
| Parsed document | `MAX_DOM_NODES` (40,000) | `check_dom_budget` runs before `resolve`, layout, and semantic walks. |
| Parsed depth | `MAX_DOM_DEPTH` (128) | Document root is level 0; checked in the same pre-layout pass. |
| Embedded PNG | `MAX_IMAGE_PIXELS` (8,000,000) | IHDR dimensions validated with checked multiplication before decode. |
| All embedded PNGs | `MAX_TOTAL_IMAGE_PIXELS` (16,000,000) | Charged per admitted image before Blitz decodes it. |

Node and depth caps reject the document. Image caps deny that image only, so the
rest of the document stays readable.

## Resource policy

| Route | Decision |
|---|---|
| Inline `<style>` and `style` attributes | Allowed. No fetch. |
| Inline static SVG shapes/text | Allowed. |
| SVG `image`/`use`/`feImage` `href` | Allowed only for `#fragment` inside the current SVG; other values are removed and recorded. |
| `data:image/png` | Allowed within the byte-derived pixel budgets. Base64 and percent-encoded payloads are decoded here. |
| `@import`, CSS `url()`, `<link rel=stylesheet>`, `@font-face` | Reach the network provider and are denied. |
| `http(s)`, `ws(s)` | Denied (`network access denied`). |
| `file:` and relative paths resolved against `glisk://document/index.html` | Denied (`filesystem access denied` or `resource scheme denied`). |
| `<iframe>` and subdocuments | Denied at the network provider; `subdocument_depth` starts at 0. Inline `srcdoc` is removed before layout. |
| Scripts | Not part of the authoring contract; no script execution path is enabled. |
| Link activation and form submission | All external navigation reaches `PolicyNavigationProvider` and is recorded as denied. |
| `#fragment` links | Handled inside Blitz as same-document scrolling; they never reach the navigation provider. |
| System fonts | Resolved by the application's font context, not by document URLs. |

`PolicyNetProvider::is_noop()` is `true` because the only admitted resource is
delivered synchronously inside `fetch`. A denied stylesheet therefore cannot
leave a pending critical resource that blocks the first paint.

Diagnostics: `ResourcePolicy::denials()`, `denial_reasons()`,
`admitted_images()`, and `admitted_image_pixels()` expose policy outcomes
without coupling rendering to terminal escape production.

## Wiring in the CLI

`spike/src/main.rs` declares `mod document;` and sends file and `-` sources
through `document::ingest_file` / `ingest_stdin` with the same `Limits::default()`
before mode selection or alternate-screen entry. The live path re-lays out the
returned document at terminal geometry; `--text` and automatic fallback use the
document for `text::extract_plain_text`. The per-module deny-all provider from
the renderer spike has been removed.

## Verification

`spike/tests/document_ingest.rs` covers:

- identical documents from the example file and the same bytes through the
  reader-shaped stdin path;
- the exact byte-cap boundary and `limit + 1` rejection;
- invalid UTF-8;
- node budget, depth boundary, and one level over the cap;
- PNG IHDR validation, per-image pixel cap, and cumulative document budget;
- external, filesystem, `about:`, non-PNG `data:`, and malformed `data:` URLs;
- a valid 1×1 embedded PNG decoding into image data with readable text;
- blocked CSS import, stylesheet, network image, file image, and iframe with a
  readable document body;
- non-fragment SVG references stripped while local `#fragment` references stay;
- external navigation denial and fragment-only local scrolling.

## Limitations

- Embedded images are PNG only. Other `data:` image types are denied, including
  SVG data URLs; inline SVG is the supported vector path.
- `srcdoc` subdocuments are stripped before layout. A document containing one
  still renders; the inline frame body is not shown.
- SVG sanitization inspects `image`, `use`, and `feImage` elements for `href`
  attributes. Fragment references are kept, but no other SVG resource loading is
  supported.
- The policy bounds image pixels, not compressed bytes beyond the overall HTML
  cap. The HTML cap already bounds an embedded data URL.
- Node and depth checks use the live Blitz tree after parsing. The parse itself
  is bounded by the 4 MiB input cap.
- Viewport raster and encoded-frame caps belong to the paint/CLI wiring, not
  this module.
- Stdin is read through the generic reader; the test suite exercises that reader
  shape rather than an interactive stdin stream.
