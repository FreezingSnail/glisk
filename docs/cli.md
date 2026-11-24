# Glisk CLI and pager integration

Bead: `glisk-2hp.8` · entry point: `src/main.rs` · tests:
`tests/cli_e2e.rs`, `tests/tmux_passthrough.rs`

The CLI is one local document reader. It accepts one `.htm`/`.html` path, `-`
for stdin, or a directory to browse; with no positional argument it browses the
current directory. URL-shaped arguments are rejected before the filesystem is
touched. The document is read through `src/document.rs` with the default 4 MiB
input cap, DOM node/depth budgets, and offline resource policy. Text extraction
and graphics share the same live `BaseDocument`.

## Modes

| Invocation | Result |
|---|---|
| `glisk report.html` | Live pager when `stdout` is a TTY and either the direct Kitty/geometry probe or the tmux pane/client check succeeds. The named file is watched and redisplayed when it changes. |
| `glisk report.html --no-watch` | Same live pager, but this session holds the revision that was opened and never reloads it. |
| `glisk -` | Same, reading the document bytes from stdin. Keyboard input comes from the terminal behind `stdout`; a pipe on stdin is fine. Stdin is never watched. |
| `glisk` | Report picker rooted at the current working directory. |
| `glisk DIR` | Report picker rooted at an existing directory. |
| `glisk report.html --text` | Plain UTF-8 text from `text::extract_plain_text`; no terminal session, tmux query, graphics, or watch. |
| Redirected `stdout` | Plain text immediately, without tmux detection or graphics. |
| Missing TTY, graphics rejection, missing geometry, probe timeout, tmux failure | Plain text for a named report; one actionable reason is printed on stderr. A picker request fails with a hint to pass a report path instead. |
| Closed downstream pipe on plain output | Quiet success, exit 0. |

Diagnostics go to stderr. Plain content and terminal graphics go to stdout.
Usage errors for unsupported flags exit 2 through `clap`; input, resource-limit,
render, and I/O failures exit 1. Normal quit, Ctrl+C, and a closed downstream
pipe exit 0.

There is no separate text picker. Redirected stdout, `glisk --text`, and
`glisk DIR --text` print a short stderr message asking for a report path and
emit no escape sequences.

## Report picker

`glisk` and `glisk DIR` discover regular `.htm`/`.html` files (case-insensitive
suffix) below the selected root. Hidden entries and every symlink are skipped.
The tree walk, visited directories, depth, and retained list are bounded; a tree
that exceeds a budget shows `partial` in the header. Only filenames and
modification metadata are read until a report is chosen. Entries sort newest
modification first, with the root-relative path as the tie breaker.

The list shows root-relative paths, the current filter, the selected row, and
the key hints, sized to the pane. Typing filters case-insensitively by relative
path. Display text is sanitized for control characters and terminal cell width;
the native path is kept for selection.

| Key | Action |
|---|---|
| Up/Down | Move the selection by one row. |
| Page Up/Page Down | Move by one visible page. |
| Home/End | First or last match. |
| Enter | Open the selected report in the normal rich pager. |
| Backspace | Remove one filter character. |
| Escape | Clear a nonempty filter; from an unfiltered list, exit. |
| Ctrl+C | Exit and restore the terminal. |

`j`, `k`, `q`, and other printable keys are filter text in the picker; they only
have report commands after a report opens.

Enter rechecks the selected path before the bounded offline ingestion. A report
removed, unreadable, or over budget leaves the list open with a one-line error.
The report watches by default and honors `--no-watch`. Escape inside a report
cancels an active search prompt first; a second Escape clears the report image,
stops its watcher, and returns to the filtered list with the previous selection.
Returning also refreshes discovery, so newly created reports appear. `q` from a
report and Ctrl+C exit Glisk. Both screens share one terminal owner, so only one
of them controls the pane at a time; exit restores the cursor, screen, and
termios.

Directory navigation, recent history, and a text-mode picker are out of scope.

## Following a local report

An interactive pager session for a named local `.htm`/`.html` file watches
that one path by default. Save the file from an editor, or let an agent
regenerate it with an atomic `rename`, and the open report redraws without a
restart. The same behavior applies to direct Ghostty and to the supported tmux
pane; no tmux option changes.

- Checks are throttled and debounced inside the existing input loop, so bursts
  coalesce and unchanged bytes do not repaint.
- Only the opened path is observed. There is no directory discovery and no
  secondary document read.
- A missing, unreadable, oversized, or unrenderable replacement keeps the last
  good report and shows a concise reload error in the status row. A later valid
  save recovers automatically.
- Reading position follows a surviving `id` on the visible block or its
  containing section, then a surviving preceding heading `id`, then clamps the
  previous scroll offset. Bottom stays bottom. Open disclosure `id`s, focused
  eligible controls, and the search query are mapped onto the new document;
  matches are rebuilt without jumping away from the restored section.
- `--no-watch` opens a snapshot session that never reloads.
- Stdin, `--text`, redirected stdout, graphics fallback, and the hidden
  diagnostic modes never start a watcher and continue to exit normally.

Author stable, unique ids on sections, headings, and disclosures so reloads can
keep the reader in place. See [`authoring.md`](authoring.md#reload-stable-ids).

## tmux setup

The pane-scoped opt-in is required; Glisk never changes tmux options:

```sh
tmux set-option -p allow-passthrough on
./target/release/glisk examples/decision-report.html
```

Run that command in each pane that should render (a split does not inherit the
pane-local option). The verified matrix is one tmux layer, tmux 3.5+ (tested
3.5a), one attached Ghostty 1.4+ display client, and a visible pane. Glisk reads
the pane's `TMUX_PANE`, the socket in `TMUX`, the stdout TTY, and these
read-only tmux values:

- effective pane `allow-passthrough` and `alternate-screen`;
- tmux version, pane id/type/size/visibility;
- the single attached client's name, type, cell pixels, control-mode flag, and
  UTF-8 flag;
- whether tmux would preserve 24-bit color for the placeholder image id.

The pane PTY supplies rows/columns. The client supplies cell pixels; Glisk does
not rely on `TERM_PROGRAM`, `TERM`, or an outer `CSI 16t` reply. If the direct
`xterm-ghostty` check fails, the automatic path prints the reason and writes
plain text:

| Failure | Diagnostic hint |
|---|---|
| `allow-passthrough` off | `tmux set-option -p allow-passthrough on` |
| `alternate-screen` off | `tmux set-option -p alternate-screen on` |
| tmux < 3.5 | Upgrade tmux. |
| client not `xterm-ghostty` / control mode / not UTF-8 | Attach Ghostty 1.4+. |
| Ghostty < 1.4 or version unresolved-and-invalid | Upgrade Ghostty. |
| zero cell pixels | The terminal did not report pixel dimensions. |
| truecolor disabled | Keep Ghostty terminfo or add `xterm-ghostty:Tc`. |
| more than one attached client | Attach exactly one display client. |
| stdout is not the pane's TTY | Run Glisk directly in the pane. |

Glisk re-checks visibility and client cell pixels at most every 500 ms and
re-uploads the latest frame after a pane/window returns to view. Pane resize,
zoom, font-size changes, scroll/search/disclosure state, and the status row stay
inside the pane. On `q`, Ctrl+C, or a handled error it deletes only its own image
and leaves the pane-local alternate screen.

Nested tmux, multiple attached clients, detach/reattach, and remote transports
are outside the initial supported matrix. Direct Ghostty, redirected stdout,
stdin, `--text`, and offline local HTML behavior are unchanged.

## Live keymap

| Key | Action |
|---|---|
| Arrows, `j`/`k`, wheel/trackpad | Scroll one step. |
| Page Up/Down, Space | Scroll one page. |
| `g`/`G`, Home/End | Top or bottom. |
| Left-click | Follow a visible fragment link or toggle a visible disclosure summary at its rendered position. The clicked control keeps keyboard focus. |
| Tab / Shift+Tab | Move focus among visible fragment links and disclosure summaries. |
| Enter | Follow the focused fragment or toggle the focused disclosure. |
| `/`, `n`, `N` | Open search, then next/previous match. |
| Escape | Cancel the search prompt, preserving the previous query and position. In a report opened from the picker, Escape returns to the report list. |
| `q`, Ctrl+C | Finish and restore the terminal. `q` in a picker-opened report exits Glisk. |

Mouse reporting is enabled with the live view and released on quit, Ctrl+C, and
handled errors. Clicks on the status row, ordinary content, and external links
are ignored; only same-document fragment links and disclosure summaries activate.

Resize is read from the terminal owner's `TIOCGWINSZ` path; the viewport reflows
with its block anchor, so reading position, focus, and open disclosures survive.
In tmux the client cell pixels and pane visibility are re-read on the bounded
interval described above. The search/status row is reserved below the image and
sanitized by the terminal owner.

## Hidden diagnostic flags

`--snapshot`, `--self-test`, `--scripted-actions`, `--scroll-sweep`, `--once`,
`--dump-dir`, `--metrics`, `--fragment`, `--toggle`, `--focus`, `--css-width`,
`--css-height`, and `--scale` remain hidden for the renderer evidence in
`docs/renderer-spike.md` and `docs/release.md`. They use the same bounded
ingestion path. `--scroll-sweep` paints every page from the top to the end and
records the largest raster/encoded frame, so a long report can be checked for
viewport-bounded memory. The old `--cell-width-px` / `--cell-height-px`
fallback was removed with the terminal owner; a missing cell measurement selects
plain text.

## Build and package

The repository root is the Cargo package `glisk`, and the only binary is
named `glisk` (`[[bin]]` in `Cargo.toml`). Build and assemble the local
macOS bundle from the pinned lockfile:

```sh
./scripts/package-macos.sh
```

That runs `cargo build --release --locked` at the repository root,
writes `dist/glisk`, and creates a deterministic
`dist/glisk-v0-macos-<arch>.tar.gz` with the example, authoring guide, CLI doc,
and checksums. Artifacts are ignored by git; the procedure and sources are
checked in.

## Tests

```
cargo test --bin glisk picker
cargo test --test cli_e2e
cargo test --test tmux_passthrough
```

The E2E suite spawns the real binary and covers file/stdin text, redirected
stdout, PTY-backed live mode with both file and piped stdin documents, probe
timeout, explicit graphics rejection, missing/URL/invalid/oversized input, and
a closed stdout pipe. The live test also exercises disclosure activation,
search inside a closed disclosure, next-match reveal, resize, and quit while
asserting image deletion, cursor restoration, and alternate-screen exit.

Picker coverage in `cargo test --bin glisk picker` checks nested discovery,
hidden/symlink exclusions, newest-first ordering, filter and Escape behavior,
empty/no-match states, cell-width truncation, selection visibility, and bounded
partial results. The five `picker_*` E2E tests cover both invocations, resize,
filtered open, an empty root, return with filter and selection preserved, image
deletion, watch and `--no-watch`, removed-after-discovery recovery, clean
Ctrl+C/`q`, and non-interactive failure without escape sequences.

`tmux_passthrough` starts an isolated `tmux -S <unique> -f /dev/null` server
for every test and owns the outer pseudo-terminal; it never contacts the user's
tmux server. It covers DCS wrapping/unwrapping, passthrough on/off, placeholder
cell and truecolor preservation, steady `a=f` frame edits on scroll, `a=T`
bootstrap on resize/recovery, two panes with independent image ids, deletion
scoped to one id, the full live pager through the tmux path, `q` and Ctrl+C pane
restoration, and the automatic fallbacks for an unsupported client, zero cell
geometry, and ambiguous multi-client attachment. A real
Ghostty 1.4 window adds the visible compositor check recorded in
`docs/evidence/tmux-ghostty.md`.
