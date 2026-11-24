# Direct Ghostty flicker evidence

Bead: `glisk-2hp.16` · code: `src/terminal.rs`, `src/main.rs` · tests:
`src/terminal.rs` unit tests, `tests/terminal_session.rs`, `tests/cli_e2e.rs`

Recorded 2026-11-24 on macOS 26.6.2 (25G83), Apple Silicon, Ghostty 1.4.0
(`ghostty +version`). The user's active tmux pane and settings were not
touched; live runs opened a separate new Ghostty instance from a repository
launcher under `target/` and let it close after the scripted session.

## Cause

Glisk only enabled non-blanking `a=f` frame edits when
`TERM_PROGRAM=ghostty` and `TERM_PROGRAM_VERSION>=1.4` were both present. The
checked-in documentation capture command explicitly ran
`unset TMUX TMUX_PANE TERM_PROGRAM` for a "direct" Ghostty run. With the program
hint stripped, the owner classified the terminal as `Other` and every steady
scroll/animation frame used a fresh `a=T` upload.

Ghostty 1.4.0 deletes the existing image and all placements at the start of an
`a=T` load for the same image id
(`src/terminal/kitty/graphics_exec.zig`, `loadAndAddImage`). A multi-chunk
viewport transmission therefore has a visible gap. `a=f,r=1,X=1` instead edits
the root frame in place and only marks the image content changed when the final
chunk arrives (`completeAnimationFrame`), so the previous pixels stay visible
for the whole transfer.

## Final gate: positive identity, not a hint guess

`TERM=xterm-ghostty` (or a program hint without a version) only decides to ask
the terminal. It never enables frame edits by itself. On the direct path, when
the environment cannot prove the capability, the existing startup probe appends
`CSI > 0 q` (XTVERSION) and reads the reply in the same bounded budget as the
cell-size and Kitty queries. Ghostty 1.4.0 answers from
`reportXtversion()` (`src/termio/stream_handler.zig`):

```
ESC P > | ghostty <version> ESC \
\x1bP>|ghostty 1.4.0\x1b\\
```

Only a reply whose name is `ghostty` and whose major/minor is at least 1.4
enables `a=f`. A pre-1.4 reply, a foreign terminal (`kitty(…)`, `WezTerm …`),
a malformed reply, or no reply at all leaves the full-frame `a=T` path. The
reply is consumed by the existing reply scanner, including split and late
arrivals; ordinary bytes in the same reads are still returned as keys.

The environment rules are:

| Environment | Evidence | Frame edits |
|---|---|---|
| `TERM_PROGRAM=ghostty` + parsed `TERM_PROGRAM_VERSION>=1.4` | positive, no probe | yes |
| `TERM_PROGRAM=ghostty` + explicit pre-1.4 or unusable version | authoritative no | no, no probe |
| non-empty conflicting `TERM_PROGRAM` (e.g. `tmux`, `WezTerm`) | not Ghostty | no |
| `TERM=xterm-ghostty`, program hints absent or version missing | needs proof | only with `ghostty >=1.4` XTVERSION |
| `TERM_PROGRAM` absent, compatible lone `TERM_PROGRAM_VERSION` | identity unproven | only with `ghostty >=1.4` XTVERSION |
| `KITTY_WINDOW_ID` present (value presence, even non-UTF8) with no Ghostty program hint | Kitty | no |

The known normal environment path does not pay the XTVERSION round trip: an
explicit Ghostty program and compatible version still decides without asking.

## Reproduction and wire evidence

Prototype (superseded): the first patch treated `TERM=xterm-ghostty` with both
program hints absent as sufficient. An isolated new Ghostty 1.4.0 window ran
the release binary through `script -q <log>` with the program hints stripped
and produced one bootstrap `a=T` plus nine frame edits (1156 `a=f` chunk
commands) across ten frames (628 chunk commands for the animation fixture).
Those wire counts describe the
prototype fallback; the review correctly rejected the missing version evidence
and the final gate requires the XTVERSION reply instead.

Final behavior is validated in two layers:

1. **Real Ghostty 1.4.0, isolated window** — the launcher strips `TMUX`,
   `TMUX_PANE`, `TERM_PROGRAM`, and `TERM_PROGRAM_VERSION` inside the Ghostty
   child, then runs:

   ```sh
   env -u TMUX -u TMUX_PANE -u TERM_PROGRAM -u TERM_PROGRAM_VERSION \
     script -qk target/live-wire.log \
     target/release/glisk examples/decision-report.html --scripted-actions \
       --metrics=target/live.metrics
   ```

   `-k` also logs keys delivered to the program, so the XTVERSION reply appears
   in the capture. The observed grid was 177 × 55 cells, 17 × 34 px cells,
   `TERM=xterm-ghostty`, with `TERM_PROGRAM` and `TERM_PROGRAM_VERSION` empty.

   | Observation | Value |
   |---|---|
   | Exact reply bytes | `\x1bP>|ghostty 1.4.0\x1b\\` (20 bytes, starts at log offset 67) |
   | `CSI > 0 q` queries | 1 |
   | `a=T,f=100` transmits | **1** (initial frame) |
   | `a=f,f=100` frame edits | **9** |
   | `a=f` chunk commands | **1156** |
   | Cleanup `a=d` | 1 |
   | Scripted frames recorded | 10 (`full_frames=1`, `frame_edits=9`) |
   | Wire log | 5,270,834 bytes; first frame 646,847 bytes |
   | Metrics | `frame_editing=true`, `xtversion=ghostty 1.4.0` |

2. **Deterministic PTY coverage** — `tests/cli_e2e.rs` and
   `tests/terminal_session.rs` emulate the replies for the same environment:

   | Case | Reply | Frames |
   |---|---|---|
   | program hints absent, `xterm-ghostty` | `>|ghostty 1.4.0` | 1 `a=T`, then `a=f` |
   | same, split reply across writes | `>|ghostty 1.4.0` | 1 `a=T`, then `a=f` |
   | same, prefix before the startup deadline, suffix after establishment | `>|ghostty 1.4.0` | every frame `a=T` (no late upgrade); suffix consumed, following key delivered |
   | same | `>|ghostty 1.3.9` | every frame `a=T` |
   | same | `>|kitty(0.40.0)` / `>|WezTerm 20240203` | every frame `a=T` |
   | same | no identity reply | every frame `a=T`, session starts/exits normally |
   | `TERM_PROGRAM=ghostty`, version `1.4.0` | no query sent | 1 `a=T`, then `a=f` |
   | `TERM_PROGRAM=ghostty`, version `1.3.9` | no query sent | every frame `a=T` |

The partial-reply case matters because the startup budget can expire after the
cell and Kitty answers are complete but while the identity reply is still
arriving; the shared scanner retains the incomplete tail. Carrying that tail
into the live input buffer makes the later suffix protocol, not ordinary keys,
and the frame path decided at startup is not revisited.

Steady frames start with
`\x1b_Ga=f,f=100,i=<id>,r=1,X=1,q=2,m=1;`, continue with
`\x1b_Ga=f,m=1;`, and finish with `\x1b_Ga=f,m=0;`. No `a=T`, delete, or
placement replacement occurs between those frames.

### Default launch, no flags

`default_launch_selects_probe_gated_frame_edits_without_flags` in
`tests/cli_e2e.rs` spawns the real binary as `glisk examples/decision-report.html`
with both program hints removed and no capability or test flags. This check
uses **PTY keys, not `--scripted-actions`**: the harness answers XTVERSION with
`>|ghostty 1.4.0`, waits for the bootstrap `a=T` frame, sends an ordinary scroll
key (`j`), requires a subsequent `a=f` edit with no extra `a=T`, then quits with
`q`. The scripted real-Ghostty run above remains the wire evidence for the
terminal capability itself.

## Automated checks

| Command | Result | What it covers |
|---|---|---|
| `cargo test --test terminal_session` | 40 passed | probe, XTVERSION accept/reject/no-reply, split/late/prefix-at-deadline reply consumption, chunk order, steady `a=f` without placement replacement, resize bootstrap, cleanup |
| `cargo test --test cli_e2e` | 31 passed, 1 ignored | real binary with stripped hints: positive, split, old/foreign identity, and no reply; default no-flag launch with a PTY scroll key; normal env path; existing first paint, resize, picker, watch, search, navigation, cleanup |
| `cargo test --test tmux_passthrough` | 23 passed | tmux framing, placeholder cells, steady `a=f`, passthrough-off plain text |
| `cargo test` | all suites passed | full regression set |

`tests/cli_e2e.rs` pins a neutral `TERM_PROGRAM=glisk-test` for ordinary PTY
runs so capability selection no longer depends on the developer's shell. The
focused tests above set or remove the variables deliberately. The PTY reply
harness now scans only new bytes plus a small overlap instead of cloning the
whole capture, so a probe with no identity reply does not slow the child.

## What was visually confirmed, and what was not

- **Protocol evidence, not a screenshot.** The host denies compositor capture
  (`screencapture` reports no display image; Screen Recording is not granted),
  so the rendered pixels were not recorded from this session. The evidence is
  the real Ghostty 1.4.0 wire stream, its parsed XTVERSION identity and cell
  probe, and the 1.4.0 source paths above.
- **Steady redraw behavior.** With positive evidence, both the hint path and
  the XTVERSION path send one bootstrap frame and then only in-place frame
  edits while scrolling and animating. Ghostty keeps the previous root-frame
  pixels until the edit's final chunk, which is the mechanism that prevents
  the old visible gap.
- **Static idle.** The synchronous loop draws only after reported input, a
  geometry change, a watch reload, a repaint request, or an active animation.
  `live_pager_no_watch_holds_the_original_revision` holds a static session for
  700 ms and asserts the graphics frame count does not change.
- **Old/absent Ghostty.** A Ghostty 1.4+ window that strips both program hints
  is now verified by the reply. A pre-1.4 or non-Ghostty terminal with
  `TERM=xterm-ghostty` can answer with its own identity or not answer at all;
  both keep the safe full-frame path, so a pager cannot be left stale by a
  quiet `a=f`.
- **Not confirmed:** the user's actual shell environment was not captured. The
  reproduced missing-hint case matches the checked-in capture instructions and
  is a verified cause of direct flicker; other launcher shapes could still
  hide the identity, in which case the fallback is deliberately full frames.
- **Remaining limitation:** tmux passthrough-off still falls back to plain text
  and no tmux option is written. Other Kitty terminals keep full-frame behavior
  by design.
