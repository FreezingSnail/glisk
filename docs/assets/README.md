# Documentation captures

These files illustrate the README:

| File | Content |
|---|---|
| `glisk-pager.png` | Still of the live pager showing `examples/tour/index.html` at its opening position. |
| `glisk-picker.png` | Still of the local report picker rooted at `examples/tour`. |
| `glisk-workflow.gif` | Picker filter → open `tour/motion.html` → pause → resume → return to the picker. |
| `glisk-tour.gif` | Trackpad scroll → search for `disclosure` → section jump → disclosure toggle. |
| `glisk-click.gif` | Left-click a fragment link → jump; left-click a disclosure summary → open its body. |
| `glisk-tmux.gif` | A named report reloading after a save inside a supported tmux pane. |

## Environment

Recorded 2026-11-24 on:

| Item | Value |
|---|---|
| Host | MacBook Pro, Apple M3 Pro, built-in Liquid Retina XDR |
| macOS | 26.6.2 (25G83) |
| Ghostty | 1.4.0 stable, Metal, `frutiger-aero` theme |
| tmux | 3.5a, one attached `xterm-ghostty` client |
| Grid | 177 × 55 cells, 17 × 34 px cells, 3009 × 1870 px pager raster |
| Font | FiraMono Nerd Font Mono 14 |
| Capture date | 2026-11-24 |

The grid and status rows are the values the running pager reported. The tmux
capture used an isolated tmux server with pane-local
`allow-passthrough on`; Glisk did not change any tmux option. The click capture
reproduced the recorded 177 × 55 grid and 17 × 34 px cells through a PTY
instead of a Ghostty window, so its status rows are the text the pager wrote
rather than a screen-buffer read.

## Method and honesty note

The pager ran in separate direct Ghostty 1.4 processes inside the verified
direct Kitty path. `TMUX` and `TMUX_PANE` were unset so the new window did not
inherit the launching tmux server. `TERM_PROGRAM` and `TERM_PROGRAM_VERSION`
were left intact: they select the non-blanking `a=f` frame-editing path without
a probe. A launcher that strips them still reaches the same path when the
direct XTVERSION probe answered `ghostty` 1.4 or newer. The report frames are
unmodified PNGs written by the live pager:

```sh
unset TMUX TMUX_PANE
./target/release/glisk examples/tour/index.html --no-watch \
  --dump-dir=/path/to/tour-frames --metrics=/path/to/tour.metrics
```

The picker screens and status rows are the exact text returned by Ghostty's
screen-buffer action, rendered with the terminal's default foreground and
background and the recorded cell geometry:

```applescript
tell application "Ghostty"
  set t to focused terminal of selected tab of front window
  perform action "write_screen_file:copy,vt" on t
end tell
```

The host denies compositor screen capture to this session
(`screencapture` returns `could not create image from display`; Screen
Recording preflight is false). These images are therefore assembled at the
pager's recorded 3009 × 1870 pixel geometry from the real frame PNGs and the
real status text, using the same font and colors. They are not a mocked
terminal: the document raster and every status string came from the running
process. A compositor screenshot would add the window chrome and title bar;
those are intentionally omitted.

To reproduce a true compositor capture on a host that grants Screen Recording:

```sh
open -na Ghostty.app --args --command=/path/to/capture-pager.sh --wait-after-command=true
# then, with Screen Recording allowed, use the numeric CoreGraphics window id:
screencapture -x -l <cg-window-id> capture.png
```

The AppleScript window id is a stable automation id, not the numeric
CoreGraphics id that `screencapture -l` expects. Use a window-list helper to
find the numeric id, or capture the full display and crop the Ghostty window.

### Click capture

`glisk-click.gif` is the one capture driven by a deterministic PTY harness
instead of a Ghostty window; the scripted path reproduces without Accessibility
input.
`scripts/capture-click-demo.sh` runs the real `glisk` binary in a 177 × 55 PTY
whose probe replies report the recorded 17 × 34 px cells, so the live pager
derives the same CSS viewport as the Ghostty grid. It then sends SGR mouse
reports at fixed cells of `tests/fixtures/click-demo.html`:

| Step | Bytes written to the pager | Cell | Status the pager wrote |
|---|---|---|---|
| Fragment link | `ESC [ < 0 ; 11 ; 14 M` then `ESC [ < 0 ; 11 ; 14 m` | column 10, row 13 | `jumped` |
| Disclosure summary | `ESC [ < 0 ; 11 ; 5 M` then `ESC [ < 0 ; 11 ; 5 m` | column 10, row 4 | `evidence expanded` |

The frames come from `--dump-dir`; the status rows are parsed from the escape
sequences the pager wrote. `scripts/render-status-row.swift` draws each row
with FiraMono Nerd Font Mono at a font size solved to a 17 px advance and the
recorded background and foreground, matching the other assets.

This is a real input path but not a hardware event: the pager receives and
handles the SGR report exactly as it would from Ghostty, and the document raster
and status strings are unmodified. The pointer itself is compositor-drawn, so
it cannot appear in the frame. Nothing is drawn to fake a click; the visible
change is the jump, the opened body, and the status row.

## Rebuilding the GIFs

Frames were scaled to 1400 px wide, quantized with an ffmpeg palette, and given
0.2–2.4 s holds so the interactions remain readable. The tour GIF uses the
trackpad scroll states at 23%, 46%, 69%, 92%, and 100%, then the search,
section-jump, and disclosure status rows. The workflow GIF uses the picker
filter, live motion frames, a paused hold, resumed frames, and the returned
picker. The tmux GIF holds the pre-save and post-save frames. The click GIF is
rebuilt end to end by:

```sh
scripts/capture-click-demo.sh
# same, with a different font install:
GLISK_CAPTURE_FONT=/path/to/FiraMonoNerdFontMono-Regular.otf \
  scripts/capture-click-demo.sh
```

The script needs `cargo`, `swiftc` (for the status-row renderer), and `ffmpeg`.

The checked-in PNGs and GIFs are the outputs; the temporary frame directory is
not committed. `--snapshot` can regenerate the document-only stills without a
live terminal:

```sh
./target/release/glisk examples/tour/index.html --snapshot=report.png \
  --css-width=900 --css-height=700 --scale=2
```

## Remaining manual capture work

- **Compositor screenshots.** This host denies Screen Recording to the session,
  so no `screencapture` image includes the Ghostty window chrome. Grant Screen
  Recording and repeat the direct-Ghostty run for a true window screenshot.
- **Physical mouse click.** Trackpad scroll is a real Ghostty mouse event in the
  tour GIF. The click demo now delivers SGR left-press reports through the PTY
  harness, so the pager's click path is captured end to end, but those bytes
  are scripted input rather than a hardware button. A compositor capture with
  Accessibility-driven clicks on a host that grants Screen Recording would add
  the visible pointer; everything else would be unchanged.
- **`--no-watch` is a negative result.** Its status row looks identical to a
  watching session, so a screenshot cannot show that a save was ignored.
  `examples/README.md` and the live-reload evidence record the command and the
  focused test (`live_pager_no_watch_holds_the_original_revision`).
