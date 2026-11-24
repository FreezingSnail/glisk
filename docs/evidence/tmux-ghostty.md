# tmux + Ghostty 1.4 live evidence

Bead: `glisk-2hp.11`. Host: macOS (darwin, Apple Silicon), Ghostty 1.4.0,
tmux 3.5a. All live work used an isolated tmux server (`-S <unique socket>
-f /dev/null`) and a new Ghostty window attached to it. The user's live tmux
server, config, and panes were not modified.

## Versions and configuration

```
$ tmux -V
tmux 3.5a
$ /Applications/Ghostty.app/Contents/MacOS/ghostty --version
Ghostty 1.4.0

# read-only inspection of the user's live server (not modified)
$ tmux list-clients -F '#{client_tty}|#{client_termname}|#{client_termtype}|#{client_cell_width}x#{client_cell_height}|#{client_flags}'
/dev/ttys013|xterm-ghostty|ghostty 1.4.0|17x34|attached,focused,UTF-8
$ tmux display-message -p -t %101 '#{pane_tty}|#{pane_width}x#{pane_height}'
/dev/ttys007|92x25
$ tmux show-options -A -p -t %101 allow-passthrough
allow-passthrough* off
```

The isolated server matched the target's relevant settings:

```sh
tmux -S "$SOCK" -f /dev/null new-session -d -s glisk-live -x 100 -y 30
tmux -S "$SOCK" set-option -p -t glisk-live allow-passthrough on
tmux -S "$SOCK" set-option -s terminal-overrides 'xterm*:Tc'
open -na Ghostty --args -e tmux -S "$SOCK" attach -t glisk-live
```

Attached client:

```
/dev/ttys015|xterm-ghostty|ghostty 1.4.0|17x34|attached,focused,UTF-8
```

The pane PTY reports `TERM=tmux-256color`, `TERM_PROGRAM=tmux`; Glisk does not
use either for mode selection.

## Protocol proof before the pager

`tests/tmux_passthrough.rs` owns the outer PTY. With passthrough **off**, the
wrapped Kitty command is dropped but placeholder text remains:

```
pane output: ESC Ptmux; ESC ESC _G... ESC ESC \ ESC \
outer gets: no ESC _G, but U+10EEEE cells and the truecolor foreground
```

With passthrough **on**, tmux strips the wrapper and un-doubles the escapes:

```
inner:  ESC _G a=T,f=100,i=658188,p=1,q=2,U=1,c=6,r=3,m=0;<base64> ESC \
outer:  ESC _G a=T,f=100,i=658188,p=1,q=2,U=1,c=6,r=3,m=0;<base64> ESC \
        ESC [38:2:10:11:12m  U+10EEEE U+0305 U+0305 ...
```

Observed facts:

- tmux 3.5a answers `CSI 16t` **inside the pane** with the attached client's
  measured cell size (`ESC[6;34;17t`), but the Kitty feature query
  (`ESC _G i=31,a=q,...`) produces no pane reply. Glisk still selects tmux
  metadata (pane TTY + client fields) rather than depending on either probe.
- tmux 3.5a preserves `U+10EEEE` as one cell and forwards its combining
  diacritics; `capture-pane -p -e` shows the exact cell contents.
- truecolor is forwarded in colon form (`38:2:r:g:b`), which Ghostty accepts;
  the semicolon form is also part of the tested acceptance.
- uploads/deletes are quiet (`q=2`), so no reply reaches the pane.
- resizing the client PTY re-emits the placeholder cells.
- two panes keep distinct image ids; deleting id A does not emit a delete for
  id B.

Run:

```
cargo test --test tmux_passthrough
# 20 tests: framing, on/off, redraw, two panes, cleanup, pager e2e, fallbacks
```

## Live Ghostty run

Command typed into the isolated pane (real Ghostty window attached):

```sh
./target/release/glisk examples/decision-report.html --metrics "$METRICS"
```

### Full grid

Pane `%0` was 177×54; the status row is reserved, so the grid was
177×53 = 9381 cells:

```
tmux capture-pane -p -e -t %0 > capture.txt
# U+10EEEE occurrences (UTF-8 f4 8e bb ae) = 9381
```

The first frame bytes were 298548 (wrapped image + placeholder grid) for a
208802-byte PNG. `docs/evidence/tmux/live-pane-0.metrics` records
`placement=tmux`, image id `6702799` (`0x6646CF`, matching the foreground color
`38;2;102;70;207`), `cols=177`, `rows=54`, `cell_width_px=17`,
`cell_height_px=34`, `cold_start_to_frame_write_ms=237.7`.

### Scroll, search, disclosures

```
before:      glisk | 0%  | ready | ready
after j j:   glisk | 7%  | ready | ready
search:      / Chrome Enter
             glisk | 0%  | "Chrome" 1/4 | ready
n:           glisk | 88% | "Chrome" 2/4 | ready
Tab … Enter: glisk | 20% | "Chrome" 2/4 | focus → jumped
Tab Enter:   glisk | 87% | "Chrome" 2/4 | evidence expanded
Tab ×7 Enter: glisk | 100% | "Chrome" 2/4 | evidence collapsed
```

### Resize, zoom, window switch

```
resize-window 120x40 → pane 120x40 → 4680 placeholders = 120×39
state preserved: 100%, "Chrome" 2/4, evidence collapsed
zoom %1            → 120x40 → 4680 placeholders
unzoom %1          → 59x40
select window 1 then 0, select pane %0
                   → 2340 placeholders still current; metrics tmux_repaints=2
```

`docs/evidence/tmux/live-pane-0.metrics` shows `frames=33`, `tmux_repaints=2`
after the away/back switches.

### Two simultaneous pagers

```
%0 60x40 → 2340 cells, foreground 38;2;102;70;207 (id 6702799)
%1 59x40 → 2301 cells, foreground 38;2;59;218;237  (id 3922669)
```

`q` in `%1`: pane command returned to `fish`; `%1` metrics
(`docs/evidence/tmux/live-pane-1.metrics`) show its own image id. `%0` stayed
live with its grid and state. Ctrl+C in `%0`: pane returned to `fish`;
`docs/evidence/tmux/live-pane-0.metrics` is complete and `frames=33`.

### stdin, `--text`, redirected output

- `cat examples/decision-report.html | glisk - --metrics …` in a pane:
  `placement=tmux`, full 2301-cell grid, `q` restored the shell
  (`docs/evidence/tmux/live-stdin.metrics`).
- `glisk examples/decision-report.html --text`: plain text, 0 bytes on stderr,
  no graphics selection.
- `glisk examples/decision-report.html > out.txt`: 85 lines / 4800 UTF-8
  bytes, 0 bytes on stderr, no ESC in the file
  (`docs/evidence/tmux/redirected.stderr`).

### Passthrough off fallback

```
$ tmux set-option -p -t %1 allow-passthrough off
$ ./target/release/glisk examples/decision-report.html
glisk: graphics unavailable: tmux support passthrough-off: graphics
passthrough is off for pane %1; enable it in that pane with
`tmux set-option -p allow-passthrough on`; using plain text
```

The pane then showed the report as plain text; no `ESC _G` and no `U+10EEEE`
appeared in the pane capture. Full stderr is in
`docs/evidence/tmux/passthrough-off.stderr`.

### Direct Ghostty still works

Launched without tmux (`env -u TMUX -u TMUX_PANE open -na Ghostty --args -e …`):

```
mode=live
placement=direct
image_id=1196181843 (0x474C4953)
cols=177 rows=55
cell_width_px=17 cell_height_px=34
css_width=1416.0 css_height=864.0 scale=2.125
cold_start_to_frame_write_ms=215.1
frames=1
```

`--once` was used so the single frame, cleanup, and exit are observable without
OS keyboard injection. Metrics: `docs/evidence/tmux/direct-ghostty.metrics`.

## Resource and cleanup checks

- placeholder grid bounded to 297 rows/cols and 1 MiB; query responses to 8 KiB
  and 250 ms; visibility re-check at most every 500 ms.
- one image per session; ids are 24-bit, non-zero, distinct from probe id 31,
  and differ between simultaneous panes (observed 6702799 vs 3922669).
- deletion is `a=d,d=I,i=<id>,q=2` for the owned id only; wrapped for tmux.
- `finish()` restores the pane's shell on `q`, Ctrl+C, and normal `--once` exit.
- no tmux setting is written by Glisk; only read-only `display-message`,
  `list-clients`, and `show-options` are issued.

## Remaining human/compositor checks

- `screencapture -x` failed with `could not create image from display`; no
  screenshot of the rendered table/chart exists in this evidence. The image
  upload and placeholder cells reached the real Ghostty client, but pixel-level
  visual confirmation requires a human at the Mac.
- No physical keyboard event was delivered to the direct Ghostty path; the tmux
  run used `send-keys` and the direct run used `--once`.
- The live run changed pane/window geometry; a Ghostty font-size zoom was not
  exercised, although the bounded client-cell re-check covers it.
- Detach/reattach, multiple attached clients, nested tmux, and remote transports
  remain outside the initial matrix.
