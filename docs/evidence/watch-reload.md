# Local report reload evidence

Bead: `glisk-2hp.12` · code: `src/watch.rs`, `src/main.rs` ·
tests: `src/watch.rs` unit tests, `tests/viewport.rs`, `tests/search.rs`,
`tests/cli_e2e.rs`, `tests/tmux_passthrough.rs`

Recorded 2026-11-24 on macOS with tmux 3.5a. The active tmux client reported
`xterm-ghostty` with `17x34` cell pixels. No tmux options were changed by
Glisk.

## Focused automated checks

| Command | Result | What it covers |
|---|---|---|
| `cargo test --bin glisk` | 29 passed | watcher debounce/burst/rejection state, path states, id-mapped disclosure and focus restore |
| `cargo test --test viewport` | 17 passed | surviving block id, heading fallback, lost-id clamp, bottom preservation |
| `cargo test --test search` | 11 passed | query rebuild, current-match retention by author id, no scroll jump, new-content matches |
| `cargo test --test cli_e2e` | 19 passed | live PTY reload, atomic replace, missing-file status, recovery, `--no-watch`, quit cleanup |
| `cargo test --test tmux_passthrough` | 22 passed | tmux pane reload on save and atomic replacement, placeholder/tmux placement unchanged |
| `cargo test` | all suites passed | existing text, ingestion, navigation, terminal, and tmux behavior unchanged |

The focused live tests are:

```sh
cargo test --test cli_e2e live_pager_follows_save_atomic_replace_and_recreation
cargo test --test cli_e2e live_pager_no_watch_holds_the_original_revision
cargo test --test tmux_passthrough tmux_pane_follows_local_report_reloads
```

## Reload and atomic replacement

`live_pager_follows_save_atomic_replace_and_recreation` opens a generated local
HTML file in the real binary through a PTY, answers the Kitty/geometry probe as
`xterm-ghostty`, then:

1. rewrites the file in place and waits for a new graphics frame;
2. writes a sibling file and `rename`s it over the watched path, then waits for
   another frame;
3. removes the file, checks the status row contains
   `reload: file missing (keeping previous report)`, and verifies the retained
   report still repaints;
4. recreates the file with valid HTML and verifies a later frame recovers;
5. sends `q` and asserts the alternate screen is left and stderr is empty.

`tmux_pane_follows_local_report_reloads` repeats the normal-save and atomic
replacement sequence in a real tmux 3.5a server with pane passthrough enabled
and a Ghostty-terminfo client attached. It then checks the exit metrics:
`watch=on`, `watch_reloads>=2`, and `placement=tmux`.

## Position, search, disclosures, and focus

`tests/viewport.rs` covers author-id anchoring directly:

- a surviving block id follows content inserted above it;
- a missing block falls back to the nearest preceding heading id;
- removing every relevant id clamps to the previous scroll offset;
- an at-bottom reader stays at the bottom of a grown revision.

`tests/search.rs` confirms a reload rebuilds matches against the new document,
keeps the current match when its author id survives, does not scroll the reader,
and reports newly added content while old content no longer matches.

`src/main.rs` has a focused unit test that maps an open `<details id>` and a
focused eligible link onto a freshly parsed document by author id, proving old
DOM-local `NodeId`s are not reused.

## Opt-out and one-shot behavior

`live_pager_no_watch_holds_the_original_revision` opens with `--no-watch`,
rewrites the file, waits 700 ms, and asserts the graphics frame count did not
change. Stdin, `--text`, redirected stdout, graphics fallback, and the hidden
diagnostics never create a watcher; their existing suites pass unchanged.

## Live-environment note

The direct and tmux checks drive real PTYs with the Ghostty terminfo path and
the bounded Kitty probe. An additional interactive screenshot of the active
user pane was not taken because it would take over the terminal session running
the work. Direct Ghostty and tmux placement behavior otherwise reuses the
existing `glisk-2hp.11` terminal-owner paths unchanged.
