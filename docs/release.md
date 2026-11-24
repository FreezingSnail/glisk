# Glisk v0 release: macOS / Ghostty 1.4+

Bead: `glisk-2hp.9` · package: `dist/glisk-v0-macos-aarch64.tar.gz` ·
binary: `dist/glisk` · evidence: `docs/evidence/release/`

This is the reproduced release record for the packaged CLI, not a design note.
The binary was built from the checked-in lockfile, exercised through its normal
entry points, and measured on the documented target. Checks that need the macOS
compositor or OS input could not be observed in this environment; they are
marked unverified with exact resolution conditions instead of being inferred.

## Scope and environment

| Item | Value |
|---|---|
| Target | Glisk v0 on macOS + Ghostty 1.4+ |
| Host | MacBook Pro, Apple M3 Pro, built-in Liquid Retina XDR (3024 × 1964) |
| macOS | 26.6.2 (build 25G83), Retina only |
| Ghostty | 1.4.0 stable (Zig 0.17.0, .fast, CoreText, Metal) |
| Rust | rustc 1.97.1, cargo 1.97.1 |
| Session | launched from an opencode PTY inside tmux; live runs used separate Ghostty instances (`TERM_PROGRAM=ghostty`, `TERM=xterm-ghostty`) |
| Fixtures | `examples/decision-report.html`; generated `dist/long-report.html` (320 sections, 5,156 DOM nodes, 165,017 bytes); `examples/offline-denied.html` |

Build and package:

```sh
./scripts/package-macos.sh
```

The script runs `cargo build --release --locked --manifest-path spike/Cargo.toml`.
The package name stays `glisk-spike`; `spike/Cargo.toml` declares one binary named
`glisk`, so `--version` prints `glisk 0.0.0`. No dependency or lockfile revision
changed. Artifacts are git-ignored; the procedure, example, guide, and evidence
are checked in.

| Artifact | Size | SHA-256 |
|---|---:|---|
| `dist/glisk` | 13,057,872 B (12.45 MiB) | `ee2c6bacacf7a7e97b0769ad51d593f1fd86d3578986b80331c73be4032d6bf0` |
| `dist/glisk-v0-macos-aarch64.tar.gz` | 5,713,550 B | `12ce8ff3387800845643115bca875e0b9c215da58c020a634ea4e9f8361f632c` |

The bundle contains `glisk`, `examples/decision-report.html`,
`docs/authoring.md`, `docs/cli.md`, `README.txt`, and `SHA256SUMS`.

Reproducibility: two consecutive `scripts/package-macos.sh` runs produced the
same binary and tarball SHA-256 (`package-before.txt`, `package-after.txt`). The
tarball uses fixed file mtimes, root ownership, ustar format, and `gzip -9n`.
The binary is unsigned; the README records the quarantine removal needed after a
download. Code signing and notarization are not part of v0.

## Reference report: narrow and wide

Headless release-binary frames, 2× scale (`--snapshot`). These are the exact PNG
payloads, and the committed images show the layout:

| Viewport | Physical raster | First-frame PNG | 5-run cold start | Median peak RSS |
|---|---|---:|---:|---:|
| narrow 420 × 900 CSS | 840 × 1800 | 259,481 B | 74.8, 75.2, 75.6, 76.0, 187.0 ms (median 75.6) | 34,193,408 B (32.6 MiB) |
| wide 1000 × 900 CSS | 2000 × 1800 | 421,477 B | 91.4, 92.5, 93.1, 93.8, 102.1 ms (median 93.1) | 43,433,984 B (41.4 MiB) |

The first narrow sample includes cold file-cache cost. Narrow top, table, and
chart frames are `narrow-*.png`; wide frames are `wide-*.png`.

## Live Ghostty

Launched as separate Ghostty 1.4.0 processes with `--window-width` /
`--window-height` in cells and `--scripted-actions`, so the binary queried and
painted through the real terminal. Every row reports `term_program=ghostty` and
`term=xterm-ghostty`; cell replies were 17 × 34 px and scale 2.125. The dumped
PNGs are frames the live process transmitted immediately before the Kitty write.
They are **not** compositor screenshots.

| Run | Grid / CSS viewport | First PNG / wire | Cold start → frame write | Action → frame write, mean | Peak RSS |
|---|---|---|---:|---:|---:|
| reference narrow | 47 × 18 / 376 × 272 | 93,915 / 125,556 B | 65.1, 132.7, 153.0 ms (median 132.7) | 7.8, 8.9, 9.4 ms | 29,884,416 B (28.5 MiB) |
| reference wide | 161 × 49 / 1288 × 768 | 428,005 / 572,012 B | 197.4 ms | 42.1 ms | 48,398,336 B (46.2 MiB) |
| long report narrow | 47 × 18 / 376 × 272 | 61,902 / 82,773 B | 162.2 ms | 10.0 ms | 42,385,408 B (40.4 MiB) |

The long run has the same raster and grid as the narrow reference but 13.6× the
DOM nodes; RSS growth is DOM/layout, not image size. Frame dumps:
`ghostty-live-narrow/` (top, table, chart, open evidence),
`ghostty-live-wide/` (top, table, chart), `ghostty-live-long/` (top and
post-action frames).
Metrics: `ghostty-live-*.metrics`.

## Long report and raster bounds

`--scroll-sweep` paints one viewport-sized frame per page from top to end and
records the largest raster and PNG. A 119-page sweep exercises the whole
document, not just one anchor.

| Run | DOM nodes | Frames | Max raster | Max PNG | Peak RSS | Sweep time |
|---|---:|---:|---:|---:|---:|---:|
| reference narrow | 378 | 4 | 1,512,000 px | 259,481 B | 34,504,704 B | 178.9 ms |
| long narrow | 5,156 | 119 | 1,512,000 px | 272,529 B | 47,579,136 B | 3,658.4 ms |
| reference wide | 378 | 3 | 3,600,000 px | 421,477 B | 43,581,440 B | 211.3 ms |
| long wide | 5,156 | 80 | 3,600,000 px | 424,189 B | 57,868,288 B | 3,680.1 ms |

Raster pixels and PNG bytes stay viewport-bounded as the document grows; the
long report painted 119 frames at the same raster size. The terminal owner
reuses one image ID for every frame and has no delete between frames, checked by
`spike/tests/terminal_session.rs`; cleanup deletes that image once on exit.

Reproduce:

```sh
./scripts/make-long-report.sh
./dist/glisk dist/long-report.html --scroll-sweep \
  --css-width 420 --css-height 900 --scale 2 \
  --metrics docs/evidence/release/sweep-long-narrow.metrics
```

## Output modes, offline behavior, and failures

All commands below are the packaged binary. Full suite:
`cd spike && cargo test --locked` → **96 passed, 0 failed** across the unit and
seven integration suites (14 CLI end-to-end tests).

| Check | Command/result |
|---|---|
| File text | `dist/glisk examples/decision-report.html --text` → 85 lines, 4,800 bytes, SHA-256 `a1875ff74164ab80dcee083a1c53db1c909f1eb20e7e2a9bda49c5d3f8101b22`, 12 headings, no ESC. |
| Redirected stdout | Same bytes as `--text` (`cmp` clean). |
| Stdin | PTY and pipe tests pass; file/stdin text equality asserted in `cli_e2e`. |
| Graphics-less TTY | `script -q /dev/null dist/glisk examples/decision-report.html` → full text, no alternate-screen enter, diagnostic `graphics unavailable: terminal probe timeout: no probe replies before deadline; using plain text` (`pty-fallback.stream`). |
| Offline fixture | `examples/offline-denied.html --text` → 426 bytes, alt text for blocked images, no ESC, stderr empty, exit 0. Snapshot 840 × 1800 PNG in 70.9 ms, RSS 30,670,848 B. External stylesheet, import, font, images, iframe, link target, and script did not block the readable body. |
| Control characters | `<p>safe ESC[31m red</p>` through stdin → `safe [31m red`; ESC removed. |
| Malformed HTML | `<p>unclosed <b>bold <i>text` → `unclosed bold text`, exit 0. |
| Oversized input | 4 MiB + 1 byte stdin → exit 1, `document input exceeds 4194304-byte cap`, no stdout. |
| Missing file | exit 1, `cannot read document: No such file or directory`. |
| Broken pipe | `dist/glisk ... --text \| head -1` → exit 0, first line only. |
| Clean environment | `env -i PATH=/usr/bin:/bin HOME=... dist/glisk ... --text` → heading output, exit 0; `--version` → `glisk 0.0.0`. |

Offline denial is enforced in-process by `spike/src/document.rs` and covered by
`spike/tests/document_ingest.rs` (CSS import, stylesheet, font, network/file
image, iframe, SVG references, navigation). No OS-level packet trace was taken;
the fixture demonstrates rendering without external resources, while the tests
assert each denial path.

## Terminal cleanup

The PTY end-to-end test drives a real child process with a probing pseudo-
terminal, then asserts:

- frames use `a=T` and one image id;
- on `q`: `a=d,d=I` delete, `?1049l` alternate-screen leave, `?25h` cursor show,
  successful exit, empty stderr;
- probe timeout and unsupported-graphics paths write plain text, never a frame,
  and never enter the alternate screen.

`cargo test --locked --test cli_e2e` passes all 14 tests, including that cleanup
assertion. Relative to actual Ghostty, the live scripted runs returned exit 0 and
closed their windows, but the terminal stream after exit was not captured.

## Actual Ghostty evidence vs emulation

| Evidence | Kind |
|---|---|
| `term_program=ghostty`, 17 × 34 px cell replies, `--scripted-actions` metrics, live transmitted PNGs | **Actual Ghostty 1.4.0** process |
| Layout, chart/table readability | Actual Ghostty frame payloads; committed PNGs are those payloads, not screen captures |
| Startup, action redraw, RSS | Actual Ghostty process |
| Navigation, search, resize, quit, image delete, alternate-screen/cursor cleanup | **PTY emulation** with deterministic probe replies in `cli_e2e` |
| Probe timeout / unsupported graphics fallback | **PTY emulation** |

## Unresolved checks and resolution conditions

1. **No OS screen capture.** `screencapture -x` returned
   `could not create image from display`; Apple events returned
   `Not authorized to send Apple events to System Events. (-1743)`.
   Resolution: grant Screen Recording and Accessibility, then capture narrow and
   wide Ghostty windows and exercise keys through the OS.
2. **No OS keyboard injection.** Tab/Enter/search were exercised by the pager's
   scripted action sequence and by PTY key bytes, not by physical key events.
   Resolution: manual Ghostty walkthrough, or grant Accessibility and inject keys.
3. **Live window resize was not exercised.** Window size was set at launch; the
   resize path is covered only by the PTY test (`80×24` → `100×30`) and headless
   viewport tests. Resolution: resize a running Ghostty window and confirm reflow,
   retained focus/disclosures/position.
4. **Shell prompt restoration was not observed in Ghostty.** Cleanup sequences
   are PTY-asserted; live runs were launched with `-e` and their windows closed.
   Resolution: run from an interactive Ghostty prompt, then confirm the prompt,
   cursor, and modes after `q`, Ctrl+C, and a forced render error.
5. **Standard-density display untested.** Only Retina 2.125 scale was measured.
   Resolution: run the probe on a 1× display and record the measured cell pixels.
6. **Unsigned, non-notarized, arm64-only artifact.** Resolution for wider
   distribution: codesign/notarize and add a reproducible x86_64 build.
7. **Multiplexer/scrollback support, scripted widgets, URL input, PNG export**
   are out of v0 scope per `FEASIBILITY.md`.

## Reproduce the main checks

```sh
./scripts/package-macos.sh
cd spike && cargo test --locked && cd ..
./dist/glisk examples/decision-report.html --text | head -3
./dist/glisk examples/decision-report.html --snapshot dist/repro-narrow.png \
  --css-width 420 --css-height 900 --scale 2
./scripts/make-long-report.sh
./dist/glisk dist/long-report.html --scroll-sweep \
  --css-width 420 --css-height 900 --scale 2 --metrics dist/repro-long.metrics
open -na Ghostty.app --args --font-size=14 --window-width=46 --window-height=17 \
  -e "$PWD/dist/glisk" "$PWD/examples/decision-report.html" \
  --scripted-actions --metrics "$PWD/docs/evidence/release/ghostty-live-narrow.metrics"
```

All write targets in the evidence commands are under `docs/evidence/release/` or
the git-ignored `dist/`.
