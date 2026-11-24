# Terminal session and Kitty viewport

Bead: `glisk-2hp.3` · module: `src/terminal.rs` · tests:
`tests/terminal_session.rs` · spec:
`FEASIBILITY.md#terminal-session-and-viewport`

This module is the pager's single terminal owner. It owns TTY identity checks,
the bounded capability/geometry probe, raw mode, the alternate screen, key
input, Kitty graphics writes, resize geometry, and cleanup. It does not load or
paint documents, own the keymap, or write plain-text output.

The module is wired into `src/main.rs` by bead `glisk-2hp.8`; the CLI
opens it only after bounded ingestion and mode selection. The integration test
also includes the module with `#[path = "../src/terminal.rs"]`.

`src/tmux.rs` is the pane-scoped tmux path used by the same owner. It is never
selected for `--text` or redirected stdout, and it never changes tmux settings.

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

On macOS, `fstat` and `poll` on the `/dev/tty` pseudo-device do not identify the
underlying pts. When `stdin` is not the stdout TTY, the owner opens
`ttyname(stdout)` instead, then relies on the shared probe as the authoritative
same-terminal check. This is what makes
`cat report.html | glisk -` interactive in the tested environment.

### Public surface

| Item | Contract |
|---|---|
| `TerminalOwner::open()` / `open_with_config` | `stdout` must be a TTY. Inside tmux it first validates the pane/client (no probe); otherwise the shared cell/graphics probe confirms reads and writes reach the same terminal. Failure is an error, never a silent fallback. |
| `TerminalOwner::from_fds(input, output, config)` | Same checks for already-open descriptors; neither descriptor is closed by the owner. Used by direct PTY tests; never selects tmux. |
| `geometry()` | Validated grid, content rows above the status row, cell pixels, physical raster, CSS viewport, and derived scale. |
| `resize` / `resize_with_cell` | Recompute and validate; a failed resize leaves the previous geometry intact. |
| `refresh_size()` | `TIOCGWINSZ`; in tmux also the bounded visibility/client re-check. Returns `None` when nothing changed. |
| `take_repaint_request()` | True once after a tmux pane becomes visible again; the pager redraws its latest frame. |
| `reprobe_cell()` | Repeat only the bounded `CSI 16t` query, retaining keys typed meanwhile. In tmux the measured client cell is retained instead. |
| `poll_input(timeout)` | Returns ordinary key bytes unchanged, consumes late/foreign protocol replies, and reports `0x03` as `interrupt`. A lone Escape is returned as a key after an idle poll. |
| `present(png)` | First frame, changed raster/placement, and recovery use a full `a=T` transmit-and-place. Same-identity frames on a verified frame-editing renderer overwrite base frame 1 with `a=f,r=1,X=1`. tmux placeholder cells are written on the first frame and whenever the grid size changes. Size limits are checked before any write. |
| `paint_status(text)` | Clears and writes the last row with the cursor preserved; strips escape sequences, C0/C1/DEL, and truncates to the grid width. |
| `finish()` | Idempotent delete-image (raw or passthrough) + show-cursor + leave-alternate-screen + restore-termios sequence. |
| `image_id()`, `placement_name()`, `tmux_summary()`, `tmux_visible()`, `environment()`, `frame_editing_supported()`, `probe_report()` | Diagnostics and metrics. Environment values never choose the mode or geometry; the explicit Ghostty 1.4+ pair is the only frame-editing gate. |

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

## tmux placement

Selection is decided before raw mode and before the alternate screen is
entered. `TMUX` and `TMUX_PANE` make the owner try `tmux::detect()` first; no
`CSI 16t` or `ESC _G` probe is sent through the pane PTY. Detection is read-only
and uses fixed arguments, a 250 ms budget per query, a response bound, and
validated fields:

1. `TMUX` socket path, numeric `TMUX_PANE`, stdout TTY name.
2. `display-message` for tmux version, pane id/TTY/size/active/window/zoom/dead,
   session id, and attached count.
3. `show-options -A -p -v ... allow-passthrough` and `alternate-screen` for the
   effective pane values.
4. `list-clients -t <session>` for exactly one client: termname, termtype, cell
   pixels, control-mode, UTF-8, pid.
5. `show-options -s terminal-overrides/terminal-features` to check whether a
   matching entry disables the 24-bit color needed by the placeholder image id.

The pane PTY (`TIOCGWINSZ`) owns rows/columns; the client owns cell pixels. The
accepted matrix is tmux 3.5+ (tested 3.5a), `TERM=xterm-ghostty`, Ghostty 1.4+
(validated when tmux resolves the termtype), one attached display client, UTF-8,
truecolor, and a supported grid. Every other case returns an actionable
`TerminalError::Tmux` before the alternate screen, and the CLI writes plain
text.

The disposable-pane check also records what a pane sees: tmux 3.5a answers
`CSI 16t` inside the pane from the attached client's cell size, while the basic
Kitty query produces no pane reply. The owner still uses the tmux metadata above
for both values, matching the design choice not to depend on an outer probe.

### Framing

Each Kitty graphics command is wrapped as
`ESC Ptmux; <inner with every ESC doubled> ESC \`. That includes every bounded
PNG chunk and the session-image deletion (`a=d,d=I,i=<id>,q=2`). Uploads and
deletes are quiet (`q=2`).

A full frame transmits and places the image: the first chunk is
`a=T,f=100,i=<id>,p=1,q=2,U=1,c=<cols>,r=<content_rows>` and later chunks are
`m=0|1;<base64>`. Re-transmitting the same id with `a=T` makes Ghostty delete
the old image and all placements at the start of the first chunk, so a visible
gap can appear during a multi-chunk transfer.

On a verified frame-editing renderer, a steady same-identity frame instead
overwrites base frame 1 in place: `a=f,f=100,i=<id>,r=1,X=1,q=2` on the first
chunk and `a=f,m=0|1` on later chunks. `X=1` replaces the base pixels (including
transparency) rather than blending. The placement and current pixels remain
visible until the final chunk. A changed raster size, cell placement, or a
hidden→visible recovery uses the full `a=T` path, which re-establishes the image
and placement. Because Ghostty treats a new direct `a=T` as a continuation
while an `a=f` load is pending, recovery first sends a scoped quiet delete
(`a=d,d=I,i=<id>,q=2`) to abort the partial load, then bootstraps. A failed
update records the same recovery state, so it is never continued as an edit.

Frame editing is gated deliberately: direct terminals must report
`TERM_PROGRAM=ghostty` with `TERM_PROGRAM_VERSION` 1.4 or newer, and the tmux
path relies on detection's single attached Ghostty 1.4+ client. A successful
basic Kitty graphics query is not treated as proof of frame editing; other
Kitty-compatible terminals keep the full-image behavior.

While scrolling at a stable grid size the existing tmux placeholder cells are
retained; rewriting them would clear the pane and flash before the new pixels
appear.

The visible image is **ordinary pane text**. On the first frame and after a
grid size change Glisk clears and homes the pane with `ESC[2J ESC[H`, sets one
truecolor foreground (`38:2:<r>:<g>:<b>`, image id in 24 bits), then writes
`U+10EEEE` + row diacritic + column diacritic per cell, row by row. tmux clips
and repaints these cells itself. Cursor motion, styles, the alternate screen,
and the status row stay pane-local. Image ids mix the process id and clock,
never 0 or the probe id, and cleanup deletes only that id.

### Bounds

| Bound | Value |
|---|---|
| tmux query response | 8 KiB per query, 250 ms |
| sync interval | 500 ms minimum |
| placeholder rows/cols | 297 (diacritic table) |
| placeholder grid output | 1 MiB |
| raster / PNG / base64 | existing 8,388,608 px / 4 MiB / 4 MiB owner limits |

### Visibility and cleanup

`refresh_size()` also performs a bounded re-check. A pane is considered visible
when the session has a client, its window is active, the pane is not dead, and
it is either unzoomed or the active pane. A hidden→visible transition requests
one repaint and forces a full transmit-and-place, because `allow-passthrough=on`
drops graphics for hidden panes and terminal storage may have been lost. Client
cell changes and pane resizes recompute geometry and likewise bootstrap a full
frame.

`q`, Ctrl+C, and handled errors run the same `finish()`: delete the owned image
through passthrough, show the cursor, leave the pane-local alternate screen, and
restore termios. With `allow-passthrough=on` a deletion issued while the pane is
hidden is dropped by tmux; Glisk only claims deletion when the target permits
it.

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

Direct path (the pane is the outer terminal):

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
| tmux detection failure | Owner was never constructed; no raw mode, alternate screen, or tmux setting was changed. Plain text follows on the normal pane screen. |
| Panic/unwind | `Drop` runs the same cleanup. |

Cleanup deletes the image only if one was transmitted (raw ESC for the direct
path, DCS-wrapped for tmux), then shows the cursor, leaves the alternate screen,
and restores the saved termios. Diagnostics must be printed after cleanup so
they remain on the normal screen.

## Tests

```
cargo test --test terminal_session
cargo test --test tmux_passthrough
```

Deterministic pseudo-terminal coverage:

- probe success, geometry values, and retention of a key typed during the probe;
- shared-budget timeout, explicit graphics negative, malformed reply, missing
  geometry, incomplete reply, with termios restored on every failure;
- chunk framing, chunk count, payload reconstruction, cursor save/restore;
- steady frame edits (`a=f`) preserve placement and reconstruct the PNG, while
  changed raster/geometry and unsupported terminals bootstrap;
- two frames reusing one image id and no delete between frames;
- Ghostty version gating for frame editing;
- status-row placement/sanitization/truncation;
- resize recomputation and rollback on impossible geometry;
- cleanup after interrupt and after a handled oversized-frame error;
- same-TTY checks for crossed and non-TTY descriptors;
- raster-budget validation.

Unit tests in `terminal.rs` cover scanner framing and status/geometry helpers.

`tmux_passthrough` owns an isolated server and the outer PTY for every test:

- wrapped/quiet Kitty transmission, DCS stripping with passthrough on, and no
  graphics with it off;
- exact placeholder cell delivery, truecolor foreground, resize redraw;
- steady scrolling completes `a=f` frame edits without `a=T`/delete/placement
  replacement or re-emitting placeholders;
- resize bootstraps `a=T`, and a returning pane aborts any pending edit before
  re-uploading the image while keeping its placeholder cells;
- two panes with distinct ids and deletion scoped to one id;
- the full binary through the tmux path, `q` and Ctrl+C pane restoration;
- fallbacks for an unsupported client, zero cell geometry, and two attached
  clients.

## Live coverage and remaining gaps

`docs/evidence/tmux-ghostty.md` records the Ghostty 1.4.0 + tmux 3.5a live run:
a real attached client (17×34 cells), complete placeholder grids, pane resize
and zoom, two simultaneous pagers, window away/back repaint, `q`/Ctrl+C shell
restoration, stdin, `--text`, redirected output, passthrough-off fallback, and
the direct Ghostty owner path.

These remain human/compositor checks:

1. A visible screenshot of the rendered table/chart inside the split pane.
   `screencapture` was denied in the test environment, so the image's *pixels*
   are established by protocol and client, not by a captured screenshot.
2. Exact cell fit and absence of any visible overlay/bleed in neighboring panes
   during rapid scrolling; the protocol test proves tmux clips placeholder cells
   and deletes only the owned image.
3. Real OS keyboard input for the direct (non-tmux) Ghostty path. The tmux live
   run used tmux `send-keys`; the direct live run used `--once`, so no physical
   key was observed.
4. A real font-size change (Ghostty zoom) as opposed to a tmux pane resize. The
   client-cell re-check is implemented and covered by the bounded sync path, but
   the live run only changed pane/window geometry.
5. External `SIGINT`/`SIGTERM` while the synchronous loop is busy. Raw mode
   turns keyboard Ctrl+C into `0x03`, but this owner does not install an async
   signal handler; a process signal during a long frame is still a gap.
6. Detach/reattach, multiple attached clients, nested tmux, and remote
   transports are intentionally outside the initial matrix.
