# Glisk CLI and pager integration

Bead: `glisk-2hp.8` · entry point: `spike/src/main.rs` · tests:
`spike/tests/cli_e2e.rs`

The CLI is one local document reader. It accepts one `.htm`/`.html` path or `-`
for stdin; URL-shaped arguments are rejected before the filesystem is touched.
The document is read through `spike/src/document.rs` with the default 4 MiB
input cap, DOM node/depth budgets, and offline resource policy. Text extraction
and graphics share the same live `BaseDocument`.

## Modes

| Invocation | Result |
|---|---|
| `glisk report.html` | Live pager when `stdout` is a TTY and the bounded Kitty/geometry probe succeeds. |
| `glisk -` | Same, reading the document bytes from stdin. Keyboard input comes from the terminal behind `stdout`. |
| `glisk report.html --text` | Plain UTF-8 text from `text::extract_plain_text`; no terminal session is opened. |
| Redirected `stdout` | Plain text immediately, without probing graphics. |
| Missing TTY, graphics rejection, missing geometry, or probe timeout | Plain text; the reason is a stderr diagnostic. |
| Closed downstream pipe on plain output | Quiet success, exit 0. |

Diagnostics go to stderr. Plain content and Kitty graphics go to stdout. Usage
errors (including no input) exit 2 through `clap`; input, resource-limit, render,
and I/O failures exit 1. Normal quit, Ctrl+C, and a closed downstream pipe exit
0.

## Live keymap

| Key | Action |
|---|---|
| Arrows, `j`/`k` | Scroll one step. |
| Page Up/Down, Space | Scroll one page. |
| `g`/`G`, Home/End | Top or bottom. |
| Tab / Shift+Tab | Move focus among visible fragment links and disclosure summaries. |
| Enter | Follow the focused fragment or toggle the focused disclosure. |
| `/`, `n`, `N` | Open search, then next/previous match. |
| Escape | Cancel the search prompt, preserving the previous query and position. |
| `q`, Ctrl+C | Finish and restore the terminal. |

Resize is read from the terminal owner's `TIOCGWINSZ` path; the viewport reflows
with its block anchor, so reading position, focus, and open disclosures survive.
The search/status row is reserved below the image and sanitized by the terminal
owner.

## Hidden diagnostic flags

`--snapshot`, `--self-test`, `--scripted-actions`, `--once`, `--dump-dir`,
`--metrics`, `--fragment`, `--toggle`, `--focus`, `--css-width`,
`--css-height`, and `--scale` remain hidden for the renderer evidence in
`docs/renderer-spike.md`. They use the same bounded ingestion path. The old
`--cell-width-px` / `--cell-height-px` fallback was removed with the terminal
owner; a missing cell measurement selects plain text.

## Tests

```
cd spike
cargo test --test cli_e2e
```

The E2E suite spawns the real binary and covers file/stdin text, redirected
stdout, PTY-backed live mode with both file and piped stdin documents, probe
timeout, explicit graphics rejection, missing/URL/invalid/oversized input, and
a closed stdout pipe. The live test also exercises disclosure activation,
search inside a closed disclosure, next-match reveal, resize, and quit while
asserting image deletion, cursor restoration, and alternate-screen exit.
