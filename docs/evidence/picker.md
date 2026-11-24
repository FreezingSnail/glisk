# Report picker evidence

Bead: `glisk-2hp.13` · code: `src/picker.rs`, `src/main.rs`,
`src/terminal.rs` · tests: `src/picker.rs` unit tests, `tests/cli_e2e.rs`,
`tests/tmux_passthrough.rs`

Recorded 2026-11-24 on macOS. The active tmux server was 3.5a with one
`xterm-ghostty` client at `17x34` cell pixels, and the Ghostty terminfo bundle
was installed. Glisk did not change any tmux option.

## Focused automated checks

| Command | Result | What it covers |
|---|---|---|
| `cargo test --bin glisk picker` | 12 passed | nested discovery, hidden/symlink/extension filtering, newest-first sort, empty/missing roots, count/depth budgets, filter + Escape state machine, Enter selection, preserved filter/path on refresh, empty and no-match rendering, cell-width truncation, control-character sanitizing, selection kept visible |
| `cargo test --test cli_e2e picker` | 5 passed | both invocations, filtered open, resize repaint, empty root, return with filter and selection, image delete, watch and `--no-watch`, removed-after-discovery recovery, second open without stale state, clean `q`/Ctrl+C, non-interactive and graphics-failure diagnostics |
| `cargo test --test tmux_passthrough tmux_pane_picker` | 1 passed | picker list and report transition in a real tmux 3.5a pane with a Ghostty-terminfo client, passthrough enabled, `picker_opens=1`, `picker_returns=1`, `placement=tmux` |
| `cargo test` | all suites passed | existing text, stdin, ingestion, navigation, search, viewport, terminal, reload, and tmux behavior unchanged |

The full run also passed the earlier watch/reload suites: 24 `cli_e2e` tests,
23 `tmux_passthrough` tests, and every unit/integration binary.

## Discovery fixtures

`tests/fixtures/picker-tree` is checked in and immutable:

```
top.htm
nested/child.html
reports/UPPER.HTML        # suffix check is case-insensitive
.hidden.html              # hidden file: skipped
.hidden-dir/inside.html   # hidden directory: skipped
notes.txt                 # wrong suffix: skipped
linked.html -> nested/child.html   # symlink: skipped
linked-dir -> nested              # symlink directory: skipped
```

`tests/fixtures/picker-empty` keeps one hidden `.gitkeep`, so it is a normal
empty root. Unit tests lower `Limits` to prove the entry and directory budgets
set `partial` instead of failing. Discovery reads `read_dir` entries and
metadata only; no report body is opened until Enter.

## Live transitions

`picker_current_directory_lists_filters_resizes_and_returns` starts the real
binary with no positional argument from the fixture tree through a PTY that
answers the Kitty/geometry probe as `xterm-ghostty`, then:

1. sees `Glisk reports` plus `nested/child.html`, `reports/UPPER.HTML`, and
   `top.htm`, and confirms hidden, symlink, and non-report names never appear;
2. resizes to `100x30` and sees the list repaint;
3. types `child`, sees the selected row `> nested/child.html`, and presses
   Enter;
4. sees a Kitty `a=T` frame and the normal report status row;
5. presses `/`, types a query, and sends Escape twice: the first cancels the
   search prompt, the second returns to `filter: child` with the same selection
   and the `a=d` image delete before the list returns;
6. presses Escape again to clear the filter, types `zzz`, and sees
   `No reports match`;
7. sends Ctrl+C and asserts alternate-screen exit (`?1049l`) and cursor
   restoration (`?25h`) with empty stderr.

`picker_empty_directory_shows_an_empty_state_and_exits` opens the checked-in
empty root, sees `No reports found`, and exits cleanly with Escape.

`picker_explicit_directory_opens_with_watch_and_recovers_missing_report` passes
a directory argument, opens `report.html`, rewrites it in place, and waits for a
new graphics frame (default watch). Escape returns to `filter: report` and
deletes the image. Removing the file after discovery and pressing Enter shows
`report removed before opening`, leaves the picker on screen, and a later
`other.html` opens normally. `q` then exits.

`picker_no_watch_holds_the_opened_revision_and_q_exits` opens from the picker
with `--no-watch`, rewrites the file, waits 700 ms, and asserts the frame count
did not change.

`tmux_pane_picker_opens_returns_and_clears_the_report_image` runs the same
list → Enter → Escape sequence inside an isolated `tmux -S <unique>` server.
The attached client is the Ghostty terminfo client with `17x34` cells; the
captured pane stream contains the picker text, `\x1b_Ga=T`, the status row,
`\x1b[?1049h`, and on return `filter: report` plus `\x1b_Ga=d`. Ctrl+C exits
and writes `mode=picker`, `placement=tmux`, `picker_opens=1`,
`picker_returns=1`.

## Non-interactive compatibility

`picker_requests_without_a_terminal_fail_cleanly` checks redirected stdout,
`glisk --text`, and `glisk DIR --text`. Each exits 1 with empty stdout and a
stderr message containing `pass a report path`; neither stream contains ESC.
The same test runs the picker through a PTY with an explicit graphics rejection:
exit 1, no alternate-screen entry, stderr contains
`picker needs a supported interactive terminal` and the report-path hint.

Existing compatibility suites pass unchanged: explicit file text output,
`glisk -` stdin text output, redirected named-file output, missing/URL/invalid/
oversized input, broken stdout pipes, live file/stdin paging, search,
navigation, resize, reload, and tmux passthrough.

## Live-environment note

The tmux check drives a real tmux server and a Ghostty-terminfo client over a
PTY, which is the same verified placement path used by `glisk-2hp.11`. An
interactive screenshot of the current user's Ghostty window was not taken
because the check would take over the terminal session running the work. Direct
Ghostty uses the unchanged owner and `present` path already verified by the
earlier renderer, tmux, and reload evidence.
