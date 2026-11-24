# Glisk

Glisk is a local HTML document pager for the terminal. It reads one local
`.htm`/`.html` file or stdin and presents it as a live, scrollable view. The
verified graphics target is Ghostty 1.4+ on macOS. Glisk outputs plain UTF-8
when graphics are unavailable, when stdout is redirected, or with `--text`.

## Quick start

```sh
cargo build --release --locked
./target/release/glisk examples/decision-report.html
./target/release/glisk examples/decision-report.html --text
cat examples/decision-report.html | ./target/release/glisk -
```

Run `glisk` with no argument to pick a `.htm`/`.html` report from the current
directory, or `glisk DIR` to search an explicit tree. Type to filter by
relative path, move with Up/Down or Page Up/Down, and press Enter to open;
Escape from a report returns to the same filter and selection, while `q` or
Ctrl+C exits. Hidden files and symlinks are skipped, and the list is bounded.

Scroll with the wheel or trackpad, and left-click a visible fragment link or
disclosure summary to follow or toggle it. The full keymap is in
[`docs/cli.md`](docs/cli.md).

A named local report is followed by default: keep Glisk open in one pane and
save or atomically regenerate the HTML in another, and the view reloads without
a restart. Reader position follows stable element `id`s, open disclosures and
search survive, and a failed replacement keeps the last good report with an
actionable status. Pass `--no-watch` for a snapshot session. Stdin, `--text`,
redirected output, and graphics fallback never watch.

Build and package the macOS bundle with the checked-in pinned lockfile:

```sh
./scripts/package-macos.sh
```

## tmux

Glisk renders inside one tmux pane when the pane and client pass a read-only
capability check. Opt in per pane; Glisk never changes tmux settings:

```sh
tmux set-option -p allow-passthrough on
./target/release/glisk examples/decision-report.html
```

Requirements for the rich path:

- tmux 3.5 or newer with the pane visible (tested with tmux 3.5a);
- Ghostty 1.4 or newer attached as the client (`TERM=xterm-ghostty`);
- exactly one attached display client for the session, not control mode;
- a truecolor client (Ghostty's terminfo, or
  `set -as terminal-overrides ',xterm-ghostty:Tc'`).

Glisk re-checks the pane on a bounded interval and re-uploads its latest frame
after a pane or window returns to view. If passthrough is off, the client or
geometry is unsupported, or the attachment is ambiguous, Glisk prints one
actionable diagnostic on stderr and falls back to clean plain text. `--text`
and redirected stdout never probe tmux or graphics. Nested tmux, multiple
attached clients, detach/reattach, and remote transports are outside the
initial supported matrix.

## Scope

Glisk is a local document reader, not a browser. It never fetches the network,
reads secondary files, runs scripts, or navigates away from the supplied
document. Author one self-contained file with inline CSS, inline SVG, and
bounded `data:image/png` assets.

## Docs

- [`docs/authoring.md`](docs/authoring.md) — supported HTML subset and report skeleton.
- [`docs/cli.md`](docs/cli.md) — modes, keymap, tmux setup, diagnostics, and tests.
- [`docs/terminal-session.md`](docs/terminal-session.md) — terminal owner, direct Kitty path, and tmux placement.
- [`docs/evidence/tmux-ghostty.md`](docs/evidence/tmux-ghostty.md) — live tmux protocol and Ghostty 1.4 evidence.
- [`docs/evidence/watch-reload.md`](docs/evidence/watch-reload.md) — live reload, recovery, opt-out, and tmux workflow evidence.
- [`docs/evidence/picker.md`](docs/evidence/picker.md) — report picker discovery, transitions, and terminal evidence.
- [`docs/release.md`](docs/release.md) — macOS packaging, evidence, and unverified checks.
- [`docs/renderer-spike.md`](docs/renderer-spike.md) — historical renderer measurements.
