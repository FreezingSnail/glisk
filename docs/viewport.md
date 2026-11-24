# Viewport painting, scrolling, and resize anchoring

Bead: `glisk-2hp.4` · module: `spike/src/viewport.rs` · tests:
`spike/tests/viewport.rs` · spec:
`FEASIBILITY.md#terminal-session-and-viewport`

The viewport owns one live Blitz document and paints the visible rectangle
through the pinned Vello CPU backend. It clamps vertical scroll to the document
range, anchors a resize to the top visible content block, and keeps focus and
disclosures because it never rebuilds the DOM. Geometry is explicit: physical
pixels, device scale, and the derived CSS viewport are passed in, so the layout
is testable without a TTY.

The module is **not wired into `spike/src/main.rs` yet**. The wiring bead adds
`mod viewport;` next to the other modules and replaces the spike's inline
`load`/`render`/scroll handling. Until then the integration test includes the
module with `#[path = "../src/viewport.rs"]`.

## Contract

```rust
let mut view = Viewport::new(document, phys_w, phys_h, scale);
match view.paint()? {
    PaintOutcome::Frame(frame) => terminal.present(&frame.png)?,
    PaintOutcome::Skipped(reason) => { /* leave the previous frame, wait */ }
}
terminal.paint_status(text)?;
```

| Item | Contract |
|---|---|
| `Viewport::new(document, w, h, scale)` | Attaches the live document and applies valid geometry. Invalid or tiny geometry stores the problem; paints skip until `resize` succeeds. Never rebuilds the DOM. |
| `paint()` | Resolves layout, clamps scroll, rasterizes the visible rectangle, encodes one PNG, and enforces both budgets. Returns `Frame` or `Skipped`. |
| `resize(w, h, scale)` / `resize_with_anchor` | Validates geometry before touching layout, anchors to the captured block, clamps the new scroll. Invalid geometry keeps the previous scroll and a pending anchor for recovery. |
| `capture_anchor()` / `visible_block()` | Public diagnostics and the split point for callers that mutate the DOM between capture and reflow. |
| `scroll_by`, `scroll_page`, `scroll_home`, `scroll_end`, `scroll_to` | CSS-pixel actions, all clamped to `0..=max_scroll()`. `false` means no scroll offset changed. |
| `document()` / `document_mut()` | The same live DOM for focus, disclosures, fragments, and search. |
| `with_limits` | Raster and encoded-PNG budgets; re-checked at paint time. |
| `limits`, `geometry`, `scroll_y`, `max_scroll` | Diagnostics and metrics. |

`Frame` carries the PNG, physical width/height, raster pixel count, scale, and
the CSS scroll offset that was painted.

## Geometry policy

```
scale        = measured cell width px / 8.0          # terminal owner
physical_w   = cols * cell width px
physical_h   = (rows - status rows) * cell height px
css_w        = physical_w / scale
css_h        = physical_h / scale
```

The caller takes these values from `terminal::Geometry::physical_size()` and
`terminal::Geometry::scale()`; the terminal owner remains the only source of
cell measurement, `status_rows`, and its own validation. This module then
re-validates explicitly before allocation:

- width and height non-zero and at least `MIN_RASTER_EDGE` (2 px);
- each dimension at most `u16::MAX` because `VelloCpuScenePainter` takes `u16`;
- `width * height` in `u64` within `max_raster_pixels` (default 8,388,608);
- scale finite and positive, and representable as a finite `f32` (the Blitz
  viewport uses `f32`);
- derived CSS width and height finite and positive.

`GeometryProblem` reports which check failed. The default raster cap matches
`terminal::MAX_RASTER_PIXELS` and `FEASIBILITY.md`.

## Painting and clipping

`paint_scene` paints the document at the explicit physical size and applies the
document's viewport scroll. Blitz clips every element to the
`0..physical_width` / `0..physical_height` viewport rectangle, so content above,
below, or beside the visible rectangle is culled rather than painted into a
document-sized surface. The raster is always viewport-sized regardless of
document height.

The PNG is bounded to `max_png_bytes` (default 4 MiB) after encoding and before
the frame is returned, so the terminal owner never receives an oversized
payload. `PaintError::PngBudget` is a handled renderer error: the pager cleans
up the terminal and falls back or reports it.

Invalid, tiny, undersized, and oversized geometry produce
`PaintOutcome::Skipped` without allocating a raster. The previous Kitty
placement stays visible, and the next valid geometry resumes painting.

## Scroll and page actions

`max_scroll` reproduces Blitz's viewport rule: the root element's
`max(size.height, scrollable_overflow_rect.bottom)` minus the CSS viewport
height, floored at zero. Every action clamps, so the top and bottom never
overscroll into blank content.

| Action | Call |
|---|---|
| line/step scroll | `scroll_by(delta_css)` |
| page scroll | `scroll_page(±1.0)` |
| home / end | `scroll_home()` / `scroll_end()` |

The pager maps arrows and `j`/`k` to `scroll_by`, Page Up/Down to
`scroll_page`, Home/End to the matching action. A `false` result means the
offset was already at the limit.

## Resize anchoring policy

Before reflow, `capture_anchor` walks semantic block candidates in document
order and records:

- the first block whose bottom is below the viewport top (the top visible
  block), or the last block when none intersects;
- the signed offset `scroll_y - block_top`;
- the nearest preceding heading, used only if the block disappears;
- whether the viewport was at the bottom.

After applying the new viewport and resolving layout, the new scroll is
`block_top + offset`, clamped to the new range. If that block is no longer
attached, the preceding heading top is used and `used_heading_fallback` is set.
At the bottom, the new maximum is used so a reader stays at the end.

Blocks are leaf candidates among `body`, `main`, `article`, `section`, `aside`,
`header`, `footer`, `nav`, `div`, `p`, headings `h1..h6`, `li`, `dt`, `dd`,
`ul`, `ol`, `pre`, `blockquote`, `figure`, `figcaption`, `table`, `caption`,
`td`, `th`, `summary`, and `details`. A wrapper is skipped when it contains
another candidate, so a paragraph wins over its container. Node identity is the
live `NodeId`; the DOM is not rebuilt, so focus, open `<details>`, and all other
interaction state survive. The walk recurses through the document tree, which
the ingestion bead bounds to depth 128 and 40,000 nodes.

## Wiring the later bead

`src/main.rs` currently owns its own document load, paint, and scrolling. The
wiring bead replaces that path with:

1. `mod document;` and `mod terminal;` plus `mod viewport;`.
2. Open the document through `document::ingest_file` / `ingest_stdin` with a
   `blitz_traits::shell::Viewport` built from the terminal geometry. Keep the
   returned `IngestedDocument` alive.
3. Open `TerminalOwner`. Take `let geometry = owner.geometry(); let (w, h) =
   geometry.physical_size(); let scale = geometry.scale();`.
4. Build `let mut view = Viewport::new(ingested.document, w, h, scale);`.
5. On every key or state change: apply pager actions through
   `view.document_mut()`, then `view.paint()`. On `Frame`, call
   `owner.present(&frame.png)` and then `owner.paint_status(text)`.
6. On `refresh_size() -> Some(geometry)` or after `reprobe_cell()`:
   `view.resize(physical_w, physical_h, scale)`; paint the resulting frame.
   The terminal owner still validates its own geometry first, so its
   `Probe`/`Geometry` errors map to text mode exactly as documented in
   `docs/terminal-session.md`.
7. On quit, interrupt, or a handled error: `owner.finish()` before
   diagnostics. `frame.png` is one owned `Vec<u8>` per frame; nothing retains
   old frames.

No `main.rs`, `terminal.rs`, `document.rs`, `Cargo.toml`, or `Cargo.lock`
change is part of this bead.

## Tests

```
cd spike
cargo test --test viewport
```

Deterministic, terminal-free coverage:

- scroll and page actions clamp at top, middle, and bottom;
- resize at top, middle, and bottom keeps the same block and offset, or the
  bottom edge;
- focus and an open disclosure survive resize on the same live DOM;
- a removed anchor block falls back to the preceding heading;
- fixed 200×120 geometry keeps raster dimensions, pixel count, PNG dimensions,
  and budgets unchanged while the DOM grows by 120 paragraphs;
- empty, tiny, oversized-dimension, oversized-raster, and non-finite-scale
  geometry skip painting and recover on valid geometry;
- a tightened raster limit is enforced at paint time;
- the encoded PNG budget is enforced before returning a frame.

## Unverified live terminal checks

These are intentionally **not** claimed by the tests above. They belong to the
live Ghostty validation bead because they need a compositor and a real
terminal window:

1. Visible placement, clipping to the status row, and replacement with no
   image accumulation during rapid scroll and resize in Ghostty 1.4.
2. Real resize/reflow at top, middle, and bottom, including a window too small
   to draw, and recovery when it grows again.
3. Focus ring and open disclosures visible across live resize.
4. Anchor calibration on a standard-density display and other font sizes; the
   spike measured only Retina `17×34` at font 14.
5. Cold-start and action-to-frame latency through the wired pager, not the
   standalone spike.
