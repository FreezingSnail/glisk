# Documentation captures

These files illustrate the README:

| File | Content |
|---|---|
| `glisk-pager.png` | Still of the live pager showing `examples/tour/index.html` at its opening position. |
| `glisk-picker.png` | Still of the local report picker rooted at `examples/tour`. |
| `glisk-workflow.gif` | Picker filter → open `tour/motion.html` → pause → resume → return to the picker. |
| `glisk-tour.gif` | Trackpad scroll → search for `disclosure` → section jump → disclosure toggle. |
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
`allow-passthrough on`; Glisk did not change any tmux option.

## Method and honesty note

The pager ran in separate direct Ghostty 1.4 processes with `TERM_PROGRAM` and
`TMUX` unset, so it took the verified direct Kitty path. The report frames are
unmodified PNGs written by the live pager:

```sh
unset TMUX TMUX_PANE TERM_PROGRAM
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
real Ghostty screen text, using the same font and colors. They are not a mocked
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

## Rebuilding the GIFs

Frames were scaled to 1400 px wide, quantized with an ffmpeg palette, and given
0.2–2.4 s holds so the interactions remain readable. The tour GIF uses the
trackpad scroll states at 23%, 46%, 69%, 92%, and 100%, then the search,
section-jump, and disclosure status rows. The workflow GIF uses the picker
filter, live motion frames, a paused hold, resumed frames, and the returned
picker. The tmux GIF holds the pre-save and post-save frames.

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
  tour GIF. The disclosure was opened with Tab+Enter, not a captured physical
  click. A synthetic SGR left-click at a rendered fragment did jump the pager,
  but the synthetic button path was not reliable enough to include as a clean
  click demo. Click hit-testing is covered by `navigation` tests and the reader
  contract; capture a real click on a host with working accessibility input.
- **`--no-watch` is a negative result.** Its status row looks identical to a
  watching session, so a screenshot cannot show that a save was ignored.
  `examples/README.md` and the live-reload evidence record the command and the
  focused test (`live_pager_no_watch_holds_the_original_revision`).
