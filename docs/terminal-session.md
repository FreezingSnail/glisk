# Terminal session and Kitty viewport

Bead: `glisk-2hp.3` · module: `spike/src/terminal.rs` · tests:
`spike/tests/terminal_session.rs` · spec:
`FEASIBILITY.md#terminal-session-and-viewport`

This module is the pager's single terminal owner. It owns TTY identity checks,
the bounded capability/geometry probe, raw mode, the alternate screen, key
input, Kitty graphics writes, resize geometry, and cleanup. It does not load or
paint documents, own the keymap, or write plain-text output.

The module is **not wired into `spike/src/main.rs` yet**. Bead `glisk-2hp.8`
owns that integration; until then the integration test includes the module with
`#[path = "../src/terminal.rs"]`.

## Integration contract

Call order for the pager:

1. Read the document and enforce the resource policy.
2. Choose plain text immediately for an explicit `--text`, redirected stdout,
   or any non-TTY condition.
3. Build a [`TerminalConfig`] (defaults are the feasibility values) and call
   `TerminalOwner::open()` or `open_with_config(config)`.
4. Render a viewport-sized PNG, then call `present(&png)` and `paint_status(text)`
   after every state change.
5. On resize, call `refresh_size()`; it returns `Some(Geometry)` only when the
   grid changed. If the font size changed, call `reprobe_cell()` and then
   `resize_with_cell(cols, rows, cell)`.
6. Call `poll_input(timeout)` in the synchronous event loop. Feed
   `InputBatch::bytes` to the pager keymap. If `InputBatch::interrupt` is true,
   quit.
7. Call `finish()` on normal quit, after `interrupt`, and on every handled error
   before printing diagnostics. `Drop` repeats the same cleanup as a backstop
   for early returns and unwinding.

`open` errors before entering the alternate screen. The caller maps every
`TerminalError::Probe` and `TerminalError::Geometry` to the plain-text path and
reports diagnostics on the normal screen.

### Public surface

| Item | Contract |
|---|---|
| `TerminalOwner::open()` / `open_with_config` | `stdout` must be a TTY. Input is `stdin` when it is the same TTY, otherwise `/dev/tty`, which must resolve to the same terminal device. Failure is an error, never a silent fallback. |
| `TerminalOwner::from_fds(input, output, config)` | Same checks for already-open descriptors; neither descriptor is closed by the owner. Used by PTY tests. |
| `geometry()` | Validated grid, content rows above the status row, cell pixels, physical raster, CSS viewport, and derived scale. |
| `resize` / `resize_with_cell` | Recompute and validate; a failed resize leaves the previous geometry intact. |
| `refresh_size()` | `TIOCGWINSZ`; returns `None` when cols/rows are unchanged. |
| `reprobe_cell()` | Repeat only the bounded `CSI 16t` query, retaining keys typed meanwhile. |
| `poll_input(timeout)` | Returns ordinary key bytes unchanged, consumes late/foreign protocol replies, and reports `0x03` as `interrupt`. A lone Escape is returned as a key after an idle poll. |
| `present(png)` | One full Kitty transmission for the stable image id, or a bounded error before any write. |
| `paint_status(text)` | Clears and writes the last row with the cursor preserved; strips escape sequences, C0/C1/DEL, and truncates to the grid width. |
| `finish()` | Idempotent delete-image + show-cursor + leave-alternate-screen + restore-termios sequence. |
| `image_id()`, `environment()`, `probe_report()` | Diagnostics and metrics. Environment values are hints only. |

## Mode outcomes

The probe sends `CSI 16t` and the basic Kitty graphics query
(`a=q,t=d,f=24,s=1,v=1`, image id 31) back to back and reads **both replies
inside one shared 250 ms budget** (configurable). Ordinary bytes read during
that window are retained and returned by the first `poll_input`.

| Outcome | `ProbeFailureKind` | Result |
|---|---|---|
| Graphics `OK` and a plausible cell size | success | Owner opens; graphics mode is used. |
| No reply before the deadline | `Timeout` | Plain text. |
| Explicit Kitty negative (`E...`) | `UnsupportedGraphics` | Plain text. |
| Graphics `OK`, no cell reply | `MissingGeometry` | Plain text. |
| Zero/implausible cell reply | `MissingGeometry` | Plain text. |
| Malformed or foreign reply | `UnexpectedReply` | Plain text. |
| Deadline with a partial escape sequence | `IncompleteReply` | Plain text. |

There is deliberately **no guessed-cell fallback**. The previous spike's
`--cell-width-px` / `--cell-height-px` fallback is not part of this owner; a
missing measurement is a text-mode decision.

## Geometry

```
scale      = cell_width_px / 8.0
physical_w = cols * cell_width_px
physical_h = (rows - status_rows) * cell_height_px
css_w      = physical_w / scale
css_h      = physical_h / scale
```

Validation before any frame: at least 2 columns, at least one content row,
cell dimensions in `1..=4096`, finite positive scale and CSS size, and raster
pixels within the 8,388,608 provisional budget. The wire frame is also bounded
to 4 MiB PNG and 4 MiB base64.

## Kitty transmission

- One image id (`0x474c4953`, `"GLIS"`) for the whole session. Every frame is
  `a=T,f=100,i=<id>,q=2,C=1,c=<cols>,r=<content_rows>` and replaces the previous
  image.
- The frame is base64 for the PNG, split into payload chunks of at most 4000
  bytes (`chunk_bytes`, ceiling 4096). Only the first chunk carries control
  keys; later chunks are `m=0|1;payload`.
- The write is wrapped in save-cursor / home / transmit / restore-cursor.
  `C=1` keeps placement from moving the cursor. The status row is painted
  separately into the last row.
- `present` validates the PNG signature and both byte bounds before writing, so
  an oversized frame cannot leave a partial transmission.

## Cleanup matrix

| Exit path | Mechanism |
|---|---|
| Normal quit | Caller calls `finish()`. |
| Ctrl+C | Raw mode delivers `0x03`; `poll_input` sets `interrupt`; caller calls `finish()`. |
| Handled renderer/output error | Caller calls `finish()` before diagnostics; the `Drop` backstop covers early returns. |
| Probe/geometry failure | Owner was never constructed past the probe; saved termios is restored before returning the error. No alternate screen was entered. |
| Panic/unwind | `Drop` runs the same cleanup. |

Cleanup deletes the image only if one was transmitted, then shows the cursor,
leaves the alternate screen, and restores the saved termios. Diagnostics must be
printed after cleanup so they remain on the normal screen.

## Tests

```
cd spike
cargo test --test terminal_session
```

Deterministic pseudo-terminal coverage:

- probe success, geometry values, and retention of a key typed during the probe;
- shared-budget timeout, explicit graphics negative, malformed reply, missing
  geometry, incomplete reply, with termios restored on every failure;
- chunk framing, chunk count, payload reconstruction, cursor save/restore;
- two frames reusing one image id and no delete between frames;
- status-row placement/sanitization/truncation;
- resize recomputation and rollback on impossible geometry;
- cleanup after interrupt and after a handled oversized-frame error;
- same-TTY checks for crossed and non-TTY descriptors;
- raster-budget validation.

Unit tests in `terminal.rs` cover scanner framing and status/geometry helpers.

## Remaining live Ghostty checks (not claimed by these tests)

These are honest gaps; the PTY tests prove protocol bytes and mode restoration,
not the compositor. They must be repeated in a real Ghostty 1.4 window:

1. Probe replies and the 250 ms shared budget under a real Kitty protocol
   terminal, including a cold start, a slow first paint, and a terminal that
   ignores `CSI 16t`.
2. Visible placement above the status row, exact cell fit, and replacement of
   the previous image with no accumulation during rapid scrolling.
3. Cursor position and shell prompt after normal `q`, real Ctrl+C, and an
   injected renderer/output failure (the earlier spike launched with
   `Ghostty -e`, so no shell prompt was observed).
4. Live resize/reflow, including the semantic scroll anchor, preserved focus and
   disclosures, and a window too small to draw (geometry error path).
5. Cell calibration on a standard-density display and at other font sizes; the
   spike measured Retina `17x34` at font 14 only.
6. Real keyboard input through the probe window (the PTY tests inject bytes, not
   OS keys), and the interaction between a key typed during the probe and the
   pager's first action.
7. Multiplexers and terminals that do not implement Kitty graphics; these must
   select text mode via the probe, not environment hints.
8. External `SIGINT`/`SIGTERM` while the synchronous loop is busy. Raw mode
   turns keyboard Ctrl+C into `0x03`, but this owner does not install an async
   signal handler; a process signal during a long frame is still a gap.
