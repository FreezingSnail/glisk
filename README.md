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

Build and package the macOS bundle with the checked-in pinned lockfile:

```sh
./scripts/package-macos.sh
```

## Scope

Glisk is a local document reader, not a browser. It never fetches the network,
reads secondary files, runs scripts, or navigates away from the supplied
document. Author one self-contained file with inline CSS, inline SVG, and
bounded `data:image/png` assets. This build falls back to text inside tmux.
Rich rendering in tmux is tracked in `glisk-2hp.11`.

## Docs

- [`docs/authoring.md`](docs/authoring.md) — supported HTML subset and report skeleton.
- [`docs/cli.md`](docs/cli.md) — modes, keymap, diagnostics, and tests.
- [`docs/release.md`](docs/release.md) — macOS packaging, evidence, and unverified checks.
- [`docs/renderer-spike.md`](docs/renderer-spike.md) — historical renderer measurements.
