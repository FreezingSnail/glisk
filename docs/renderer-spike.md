# Renderer spike: Blitz + Vello CPU + Kitty in Ghostty

Bead: `glisk-2hp.1` · fixture: `examples/decision-report.html` · prototype: `spike/`

This spike answers one question: can a live Blitz document, rasterized by Vello CPU
and presented as one viewport-sized Kitty image, carry the reference report inside
Ghostty 1.4 on macOS at acceptable latency? It does for the tested Retina viewport.
Several interaction checks remain manual because macOS denied screen capture and
Apple-event automation in this environment. Those items are marked **unverified**
below rather than estimated.

There is no Kitty terminal dependency. Ghostty implements the Kitty graphics
protocol; the prototype writes the `a=T,f=100` transmit/display sequences itself.

## Environment

| Item | Value |
|---|---|
| Host | MacBook Pro, Apple M3 Pro (14-core GPU) |
| macOS | 26.6.2 (build 25G83) |
| Display | built-in Liquid Retina XDR, 3024 × 1964, main, Mirror off |
| Density tested | Retina only. Standard-density display was not tested. |
| Terminal | Ghostty 1.4.0 stable (`--version`: Zig 0.17.0, build mode `.fast`, app runtime none, CoreText font engine, Metal renderer) |
| Ghostty font | default 14 (FiraMono Nerd Font Mono) |
| Rust | rustc 1.97.1 (8bab26f4f 2026-07-14), cargo 1.97.1 |
| Shell / session | zsh, launched from tmux 3.x into a separate Ghostty window for live runs |

The current session itself was a tmux client (`TERM_PROGRAM=tmux`). All live evidence
came from separate Ghostty windows (`TERM_PROGRAM=ghostty`, `TERM=xterm-ghostty`),
not the tmux pane.

## Pinned crates

`spike/Cargo.lock` is checked in. Direct pins:

| Crate | Version | Why |
|---|---:|---|
| `blitz-dom` | `=0.3.0-beta.4` | headless document, style, layout, `<details>`, fragments |
| `blitz-html` | `=0.3.0-beta.4` | HTML parser wrapper |
| `blitz-paint` | `=0.3.0-beta.4` | `paint_scene` into an AnyRender scene |
| `blitz-traits` | `=0.3.0-beta.4` | `NetProvider`, `Viewport`, `ColorScheme` |
| `anyrender_vello_cpu` | `=0.18.0` | Vello CPU backend implementing AnyRender 0.14 |
| `vello_cpu` | `=0.3.0` | `Pixmap::into_png` (enables the `png` feature) |
| `crossterm` | `=0.29.0` | raw mode, alternate screen, terminal size |
| `clap` | `=4.6.7` | arguments |
| `base64` | `=0.22.1` | Kitty payload |
| `libc` | `=0.2.191` | `poll(2)` for keys and `getrusage` for peak RSS |

Compatibility note: `blitz-paint 0.3.0-beta.4` requires `anyrender ^0.14.0`.
`anyrender_vello_cpu 0.14.0` still targeted AnyRender 0.11, so it cannot pair with
this Blitz build. `anyrender_vello_cpu 0.18.0` targets AnyRender 0.14 and was used.

## Document authoring subset verified by this fixture

`examples/decision-report.html` is one file with inline CSS and one inline SVG. It uses
no script, no external stylesheet, no external font, no network image and no subdocument.

Rendered successfully by the pinned build:

- block flow, flex-wrap cards with `flex: 1 1 <basis>`, `min-width: 0`, `gap`
- borders, border radius, solid backgrounds, background/text colors
- a headered comparison table (`caption`, `thead`, `th[scope=col]`, fixed layout,
  wrapping cells) at narrow and wide widths. The pinned build does **not** reproduce
  the old `kittyhtml`-era empty-`<thead>` defect.
- inline SVG shapes, lines, dashed target line, text and `viewBox` scaling
- same-document links with unique `id` targets
- two native `<details>/<summary>` disclosures, including `open` state rendering
- visible `a:focus` / `summary:focus` outline
- a colophon and long multi-screen flow

Observed limits and authoring rules:

- SVG text does not inherit the document font stack; it paints in the SVG default
  serif face. Put an explicit `font-family` on SVG `<text>` when this matters.
- Avoid `overflow: auto`, sticky positioning and scripted widgets; they are not part
  of the verified subset.
- Keep tables at four or fewer columns and allow word wrapping. The table is readable
  at 328–368 CSS px, but each cell becomes narrow.

## Terminal geometry and scale

The prototype opens one alternate screen, reads rows/columns with `TIOCGWINSZ`, and
probes cell pixels with `CSI 16t` (`ESC[16t` → `CSI 6;height;width t`) with a 300 ms
deadline. Ghostty 1.4.0 answered immediately. The pty run without a terminal emulator
timed out and used the fallback, which is why its cold-start number includes ~300 ms.

Calibration used by the prototype:

```
scale        = cell_width_px / 8.0          # start from 8 CSS px per column
physical_w   = cols * cell_width_px
physical_h   = (rows - 1) * cell_height_px   # one row reserved for status
css_w        = physical_w / scale
css_h        = physical_h / scale
```

Measured in Ghostty at the default font:

| Live run | cells | cell px | scale | CSS viewport | physical raster |
|---|---:|---:|---:|---:|---:|
| narrow (`--font-size=14`) | 46 × 17 | 17 × 34 | 2.125 | 368 × 256 | 782 × 544 |
| observed wide/fullscreen | 177 × 55 | 17 × 34 | 2.125 | 1416 × 864 | 3009 × 1836 |
| font test (`--font-size=22`) | 30 × 11 | 26 × 53 | 3.25 | 240 × 163 | 780 × 530 |
| font test (`--font-size=16`) | 41 × 15 | 19 × 38 | 2.375 | 328 × 224 | 779 × 532 |

The device scale is **derived from measured cell pixels**, not assumed to be 2.
The 17 px cell width happens to give 2.125 here. A standard-density display must be
measured again.

## Live protocol behavior

- One image ID (`0x474c4953`, "GLIS") is reused for every frame.
- Each frame is `a=T,f=100,i=<id>,q=2,C=1,c=<cols>,r=<rows-1>`, base64 in ≤4000-byte
  chunks with `m=1` on all but the last, cursor placed at home first.
- The cursor is preserved (`C=1`); the last terminal row is then painted as a status
  line (`CSI <rows>;1H`, clear line, text).
- Exit sends `a=d,d=I,i=<id>` to delete the image, shows the cursor, leaves the
  alternate screen, disables raw mode, and flushes.
- A deny-all `NetProvider` is installed with `is_noop = true`, and the document base
  URL is `glisk://report/reference.html`. Network, file, font, image, CSS-import and
  navigation fetches are never served. A test document with an external image still
  renders, proving a denied optional asset does not block the frame.

The pty capture shows the actual escape sequence order, including alternate-screen
enter/leave and image delete:

```
docs/evidence/pty-live.metrics
```

## Measurements

All timings are `Instant` deltas measured inside the release binary. "Cold start" is
from the first line of `main` (after exec/dyld) to the first complete frame flush; it
excludes process launch. "Action to frame write" measures the same handlers used by
the key loop, but actions are scripted in-process (`--scripted-actions`), not delivered
by the OS keyboard. Frame write is a proxy for display, not proof of scan-out.

### Live Ghostty

| Metric | Narrow (46×17) | Wide observed (177×55) |
|---|---:|---:|
| Cold start → first frame write | **138.5 ms** | **186.5 ms** |
| Scripted action → frame write, mean (9 actions) | **7.7 ms** | **49.4 ms** |
| Scripted action → frame write, last | **9.5 ms** | **44.4 ms** |
| First PNG / Kitty frame bytes | 82,799 / 110,696 | 484,005 / 646,843 |
| Peak RSS | **29,048,832 B (27.7 MiB)** | not recorded (older run, no RSS field) |
| CSS viewport | 368 × 256 | 1416 × 864 |
| Cell pixels / scale | 17 × 34 / 2.125 | 17 × 34 / 2.125 |

Additional cold-start samples from live Ghostty windows:
59.7, 62.0, 70.3, 95.4, 127.6, 133.2, 138.5, 141.7, 160.3, 166.8, 184.2, 186.5, 191.0 ms.
Window size was not fully controllable from the CLI, so the sample's viewport is
recorded with it.

### Headless snapshot (same renderer, no terminal)

Five runs each, release build, 2× device scale:

| Metric | Narrow 420×900 CSS (840×1800 px) | Wide 1000×900 CSS (2000×1800 px) |
|---|---:|---:|
| Cold start → PNG, median | **68.6 ms** (68.5–112.5) | **84.6 ms** (83.5–85.2) |
| Peak RSS, median | 33,980,496 B (32.4 MiB) | 43,302,912 B (41.3 MiB) |
| PNG size | 259,481 B (top frame) | 421,477 B (top frame) |

Raw samples: `docs/evidence/snapshot-samples.txt` and `snap-*.metrics`.

### Binary

```
-rwxr-xr-x  12,993,648 bytes  spike/target/release/glisk-spike   (12.39 MiB)
```

Built with `strip = true`, `lto = "thin"`, `codegen-units = 1`.

## Evidence gallery

Live Ghostty process, narrow (46×17 cells, 368 CSS px). These PNGs are the exact
frames the prototype dumped immediately before transmitting them to Ghostty. They are
not screen captures of Ghostty's Metal compositor.

| Frame | Shows |
|---|---|
| `docs/evidence/ghostty-frames-narrow-final/frame-001.png` | top cards and nav |
| `.../frame-005.png` | comparison table, headered, narrow |
| `.../frame-006.png` | inline SVG chart and text summary, narrow |
| `.../frame-009.png` | evidence section |
| `.../frame-010.png` | first disclosure open after scripted activation |

Wide observed run (177×55 cells, 1416 CSS px), same caveat. This run predates the
final cold-bar label fix; the table and chart otherwise match the fixture.

| Frame | Shows |
|---|---|
| `docs/evidence/ghostty-frames-wide-observed/frame-001.png` | top cards, wide |
| `.../frame-003.png` | comparison table, wide |
| `.../frame-006.png` | chart and summary, wide |

Headless final render (includes the fixed `1230 ms` label):

| File | Shows |
|---|---|
| `docs/evidence/narrow-top.png` | narrow top |
| `docs/evidence/narrow-table.png` | narrow table |
| `docs/evidence/narrow-chart.png` | narrow chart + summary |
| `docs/evidence/wide-top.png` | wide top |
| `docs/evidence/wide-table.png` | wide table |
| `docs/evidence/wide-chart.png` | wide chart + summary |

## Reproduce

Build:

```sh
cd spike
cargo build --release
```

Headless frame, narrow and wide:

```sh
./target/release/glisk-spike ../examples/decision-report.html \
  --snapshot ../docs/evidence/narrow-chart.png \
  --css-width 420 --css-height 900 --scale 2 --fragment chart

./target/release/glisk-spike ../examples/decision-report.html \
  --snapshot ../docs/evidence/wide-chart.png \
  --css-width 1000 --css-height 900 --scale 2 --fragment chart
```

Headless interaction/resize assertions (anchor scroll, disclosure open, focus and open
state retained across viewport change):

```sh
./target/release/glisk-spike ../examples/decision-report.html --self-test \
  --metrics ../docs/evidence/self-test.metrics
```

Live Ghostty window with deterministic scripted actions and frame dumps:

```sh
open -na Ghostty.app --args --font-size=14 --title=glisk-spike \
  -e "$PWD/spike/target/release/glisk-spike" \
     "$PWD/examples/decision-report.html" \
     --scripted-actions \
     --dump-dir "$PWD/docs/evidence/ghostty-live" \
     --metrics "$PWD/docs/evidence/ghostty-live.metrics"
```

Replacing `--font-size=14` with 14 / 16 / 22 changes the cell grid and therefore the
CSS width. `--window-width` / `--window-height` did **not** change the grid in this
environment. Resize the window manually for a true resize test.

Interactive manual check (required for acceptance on another machine):

```sh
./spike/target/release/glisk-spike examples/decision-report.html
```

1. `Tab` / `Shift+Tab`: focus moves through nav links and both summaries, with a
   visible outline.
2. `Enter` on "Comparison" jumps to the table; `Enter` on "Evidence 1" opens it.
3. `j` / `k` / arrows / Page Up/Down: scroll. `g` / `G`: top / bottom.
4. Resize the Ghostty window while a disclosure is open: table reflows, disclosure
   stays open, focus is unchanged, scroll offset is retained.
5. `q` or `Ctrl+C`: the image disappears, the alternate screen is left, the cursor
   returns, and the shell prompt is intact.

Pseudo-terminal framing check:

```sh
script -q /dev/null spike/target/release/glisk-spike \
  examples/decision-report.html --once \
  --metrics docs/evidence/pty-live.metrics
```

## Acceptance status

| Requirement | Status | Evidence |
|---|---|---|
| Reference report checked in | **Verified** | `examples/decision-report.html` |
| Live Blitz document + Vello CPU + Kitty in Ghostty 1.4 | **Verified** | live metrics `term_program=ghostty`; frames dumped from live process |
| Headered table readable narrow | **Verified** | live narrow frame 005; headless `narrow-table.png` |
| Headered table readable wide | **Verified** | live wide frame 003; headless `wide-table.png` |
| Chart readable narrow | **Verified** | live narrow frame 006; headless `narrow-chart.png` |
| Chart readable wide | **Verified (live frame predates one label move)** | live wide frame 006; headless `wide-chart.png` |
| Keyboard anchor activation | **Not verified with OS keys** | scripted `Anchor` uses the same document call; headless self-test scrolls to `#comparison` |
| Keyboard disclosure activation | **Not verified with OS keys** | scripted `Toggle` renders live frame 010; headless self-test asserts `open` |
| Resize retains state | **Not verified live** | headless self-test asserts disclosure and focus survive a 420→1000 CSS viewport change; scroll offset retained, semantic re-anchor unimplemented |
| Shell restoration | **Partially verified** | pty capture shows alternate-screen leave/delete; live windows were launched with `-e`, so no shell prompt was observed |
| Binary size | **Verified** | 12,993,648 bytes |
| Cold startup → frame write | **Verified** | 138.5 ms narrow, 186.5 ms wide observed |
| Key → frame write | **Scripted only** | 7.7 ms mean narrow, 49.4 ms mean wide observed |
| Peak memory | **Verified narrow** | 29,048,832 B live; 34.0/43.3 MB headless narrow/wide |
| External resources blocked | **Verified by construction + test** | deny-all provider, `glisk://` base URL, inline-asset render test |
| Unresolved core behavior documented | **Verified** | gaps below |

## Unresolved core behavior and resolution conditions

1. **OS keyboard injection and screen capture were blocked.** `screencapture` returned
   "could not create image from display"; AppleScript System Events returned
   "Not authorized to send Apple events". Resolution: grant Screen Recording and
   Accessibility to the test harness, or run the manual checklist above. Do not accept
   the keyboard rows on scripted actions alone.

2. **Live window resize was not exercised.** Resolution: resize a Ghostty window while
   the prototype is running and confirm table reflow, open disclosure, focus and scroll.
   The headless API test is necessary but not sufficient.

3. **Reading position is retained as an absolute scroll offset, not a semantic block
   anchor.** A resize keeps focus, disclosures and the offset (clamped), but the top
   text block can drift. Resolution condition: record the top visible block id and its
   offset before reflow, then re-anchor after layout; verify at top/middle/bottom and at
   a window too small to draw.

4. **Shell restoration was not observed through a shell.** Live runs used
   `Ghostty -e <binary>`, which closes the window after exit. The pty capture proves the
   escape-sequence cleanup. Resolution: run from an interactive prompt and confirm the
   prompt, cursor and modes, including `Ctrl+C` and a forced render error.

5. **Wide Ghostty viewport was not CLI-reproducible.** `--window-width` /
   `--window-height` did not change the grid; one 177×55 fullscreen run was captured.
   Resolution: fix the window size with a dedicated Ghostty config/profile or run the
   manual wide test at the default size, then record the cells and cell pixels.

6. **SVG text does not inherit CSS font-family.** Resolution condition: an author-facing
   rule (explicit `font-family` on SVG text) is documented, or a pinned build is found
   that inherits fonts; then update the authoring subset.

7. **Standard-density display / non-Retina scale untested.** Resolution: repeat the
   `CSI 16t` calibration and the frame readability checks on a 1× display.

8. **Frame write is not display confirmation.** No vsync/compositor observation was
   possible. Resolution: a screen recording or high-speed capture makes display latency
   observable; treat the write timings as an upper-bound proxy for input handling.

9. **Direct placement inside tmux is out of scope.** The tmux client does not pass
   graphics through by default and needs Unicode placeholders or passthrough. This
   spike targets direct placement in Ghostty only.

## Performance targets implied by this run

- Keep first frame under 250 ms at the tested Retina viewports; 139 ms narrow and
  187 ms wide observed leaves headroom but the wide case is close.
- Keep scripted redraw under 50 ms at a 1416 CSS px viewport; 49.4 ms mean is at the
  edge. Optimize wide-viewport raster or lower the default text scale before adding
  features.
- Keep peak RSS around 30–45 MB for viewport-sized rasters; long-report DOM growth
  still needs the separate long-variant measurement from the release checks.
