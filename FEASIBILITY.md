# Glisk v0 Plan and Feasibility Research

Planned on November 23, 2026. Implementation and live terminal validation are pending.

## v0 plan

An agent writes a self-contained HTML report and gives the reader `glisk report.html`. Glisk opens a live pager in the terminal where the reader can scroll, jump to sections, expand evidence, and search the report. The first graphics target is Ghostty 1.4+ on macOS. Distribution is one Rust executable with the renderer compiled in.

Track implementation through Beads epic `glisk-2hp`. Use `bd ready --parent glisk-2hp --type task` to find the next available task.

### Reader and author contract

| Command or action | v0 behavior |
|---|---|
| `glisk report.html` | Open the live pager when graphics and a controlling terminal are available. Fall back to readable plain text when graphics are unavailable or stdout is redirected. |
| `glisk -` | Read the document from stdin. Read pager input from the controlling terminal when available. |
| `glisk report.html --text` | Write plain UTF-8 text suitable for piping and copying. Preserve headings, lists, table content, disclosure text, and image descriptions. |
| Arrows, `j`/`k`, Page Up/Down | Scroll one step or page at a time. |
| `Tab`, `Shift+Tab`, `Enter` | Move visible focus between section links and disclosures, then activate the focused element. |
| `/`, `n`, `N` | Search document text and jump between matches. Reveal a matching block inside a closed disclosure when the reader jumps to it. |
| Resize | Reflow to the new terminal width and preserve the reading position, focus, and open disclosures. |
| `q`, `Ctrl+C` | Return to the shell with the original terminal state restored. |

The reference document is a multi-screen decision report with responsive cards, a comparison table with headers, a static inline SVG chart with a caption and text summary, in-page links, and two collapsible evidence sections. Document the HTML and CSS patterns verified by that fixture so an agent can author a new report from the example. Use inline CSS and embedded assets. Navigation stays within the document.

Follow-on scope includes scripted widgets, URL input, general browser compatibility, inline scrollback previews, PNG export, graphical tmux support, and additional platform builds.

### Rust stack

| Component | Choice |
|---|---|
| CLI | Cargo binary with `clap` for arguments. [clap](https://docs.rs/clap/) |
| Document and layout | `blitz-html`, `blitz-dom`, and `blitz-paint`. Keep one live document for layout, focus, disclosure state, search, and text extraction. [Blitz architecture](https://github.com/DioxusLabs/blitz#architecture) |
| Rasterization | `anyrender_vello_cpu` and a PNG encoder. Rasterize the visible viewport on the CPU. [Blitz paint API](https://docs.rs/blitz-paint/) · [Vello CPU backend](https://docs.rs/anyrender_vello_cpu/) |
| Terminal | `crossterm` for input, resize, and terminal modes. A small Kitty graphics writer presents and replaces one image on the alternate screen. [Crossterm](https://docs.rs/crossterm/) · [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) |

Measure terminal cell pixels for raster dimensions and track CSS viewport dimensions separately. Establish the scale on the target during the spike. Run capability and geometry probes with deadlines in interactive graphics mode. Fall back to text when usable graphics or geometry cannot be confirmed. Replace the placement on each redraw and clean it up on exit. Restore raw mode, cursor visibility, and the normal screen on quit, interrupt, and handled errors.

Apply the resource policy in both output modes. Load the supplied document plus bounded embedded assets. Deny secondary filesystem access, network requests, and external navigation through the renderer's resource and navigation paths. Bound input bytes when reading and DOM depth and node count before layout. Check image and viewport dimensions before allocation. Cap the frame encoder's output before transmission. Sanitize document text before writing it to the terminal. [Blitz document configuration](https://docs.rs/blitz-dom/latest/blitz_dom/struct.DocumentConfig.html)

### Milestones

1. **Prove the live renderer.** Build the reference report and pin a compatible set of Blitz and AnyRender crates. Open it through the live viewport in Ghostty with external resources blocked. Verify the chart and table, keyboard activation of a disclosure and an anchor, resize with state intact, and clean exit. Try narrow and wide windows on Retina and standard-density displays. Record release binary size, cold time to first paint, action-to-redraw time, and peak memory for the same fixture. Use the results to set performance targets and finalize the supported authoring subset. [Blitz CSS status](https://blitz.is/status/css) · [Blitz element status](https://blitz.is/status/elements)
2. **Complete the CLI and pager.** Wire file and stdin input, plain-text output, capability fallback, search, and the full keymap into the same document path. Use legible default page colors and honor author CSS. Enforce the full resource policy before opening generated documents. Verify text fallbacks and resource limits through the normal CLI.
3. **Package and verify v0.** Ship the macOS release binary with a short usage guide, the example report, and the verified authoring subset. Run the release checks below against the packaged binary.

### Release checks

- The reference report opens from a file and stdin. Cards and table headers stay readable at the tested widths, and the chart has a useful plain-text description.
- In a single session, a reader can follow a section link, expand evidence, find text in a closed disclosure, resize, and quit. The reading position and expanded state survive resize. Quit, interrupt, and an injected renderer error restore the shell.
- A long report can be read from beginning to end while raster memory tracks the viewport. Redraws replace the active placement without accumulating old images.
- Redirected stdout, `--text`, and a terminal without graphics produce usable plain text without terminal control sequences. Malformed HTML renders safely or reports a readable error. Inputs beyond a resource limit report that limit and exit with an error. Automated checks confirm that CSS, images, fonts, and links cannot trigger external loads and that text output strips terminal control characters.

### Input and output

Accept one local path or `-` for stdin. Read the selected source as UTF-8 with a byte cap. Validate the document before switching the terminal into an alternate screen. Keep diagnostics on stderr. Usage errors exit with status 2, and input, resource-limit, render, and I/O failures exit with status 1. Normal output and a closed downstream pipe exit successfully. A blocked optional asset leaves the rest of the document readable, using alternative text where available.

Explicit `--text` and redirected stdout select plain text immediately. For graphics, open the controlling TTY and confirm it is the terminal receiving stdout. Pass that terminal to the input loop so `cat report.html | glisk -` can still be interactive. Missing TTY, unsupported graphics, incomplete geometry, or probe timeout select the same plain-text path. Keep mode selection independent of the HTML source kind.

### Document and terminal boundaries

Keep input and mode selection, the Blitz document, and the terminal session separate. The document owns the live DOM, stable node identities, layout, and embedded assets in both modes. The pager owns scroll and search state and translates terminal keys into document actions. The terminal session owns raw mode, the alternate screen, terminal replies, and graphics writes. Keep the event loop synchronous for v0 and repaint after state changes. Coalesce queued resize events and resolve layout only when the viewport or document layout changes. Blitz resolves styles and layout before painting a viewport-sized scene. [Blitz DOM API](https://docs.rs/blitz-dom/latest/blitz_dom/struct.BaseDocument.html) · [Blitz paint API](https://docs.rs/blitz-paint/latest/blitz_paint/fn.paint_scene.html)

#### Terminal session and viewport

- Verify basic graphics support and cell geometry on the same TTY, with a shared provisional 250 ms probe budget. Read replies and keys through one input owner so ordinary keypresses are retained. Treat environment variables as hints. Keep text output outside this session, and check macOS/Ghostty behavior early in the spike.
- Reserve one terminal row for search and status. Track terminal rows and columns, device pixels per cell, and CSS viewport size separately. Start with 8 CSS pixels per terminal column and compute device scale from measured cell width. Derive CSS height from the pixel height available above the status row and the same scale. Recompute geometry on resize and validate every dimension before allocating a frame. Use the spike to calibrate text size on Retina and standard-density displays. [Kitty window size and placement](https://sw.kovidgoyal.net/kitty/graphics-protocol/#getting-the-window-size)
- Paint the visible document rectangle to one bounded PNG. Place it at the viewport origin with explicit cell dimensions, stable image ID, and cursor preservation. Write each complete Kitty transmission in order, then paint the status row. Replace the prior image as the view changes. On exit, delete the session's image and restore saved terminal modes. Cleanup must run for normal quit, Ctrl+C, and handled renderer or output errors. Keep error diagnostics visible on the normal screen. [Kitty image layout](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout)
- Keep the document tall and the raster viewport-sized. Clamp scroll to the available document range. Save the top visible content block and its offset before reflow, then anchor the new scroll position to that block. If it disappears, prefer the nearest surviving preceding heading. Preserve focus and expanded disclosures through reflow. Cover top, middle, and bottom positions and a window too small to draw.

#### Navigation and search

- Tab through visible fragment links and disclosure summaries in document order, with reverse traversal on Shift+Tab. Scroll the focused control into view and make focus visible. Enter follows the current-document fragment or toggles the focused disclosure. Use the decoded fragment to find its exact HTML `id`, reveal any enclosing disclosures, and position the target in view. An unknown fragment produces a status message and preserves the current view.
- Walk semantic text blocks in document order and associate them with live DOM nodes. Combine inline text within a block so a search can span styling boundaries. Skip script, style, template, and explicitly hidden content. Include text in closed disclosures and expand ancestors only when the reader jumps to a match. Report the current match count and move the matched block into view. Use case-insensitive matching, wrap on `n` and `N`, and let Escape dismiss the search prompt. Keep the previous query when a new search is cancelled.
- Use the same semantic walk for `--text`. Preserve headings, paragraphs, nested lists, preformatted code, row and header order, caption and image text, and disclosure bodies. Show captions and chart summaries as authored prose. Fall back to semantic HTML order when CSS changes the visual layout. Normalize output breaks and strip document-supplied terminal controls, including ESC, C0, DEL, and C1 characters. Use app-owned line breaks and indentation.

### Offline authoring and resource limits

The authoring contract covers an HTML file with inline CSS, system fonts, static inline SVG shapes and text with a readable caption or summary, and bounded `data:image/png` images. Use native links and disclosures for interaction. Keep examples responsive with flexible card widths, flowing labels, and a table that fits the tested narrow viewport. Include headings with unique IDs, labeled links, meaningful image text, and a summary for data conveyed through color or graphics. Pin the verified HTML and CSS subset from the reference report.

Configure the render path to deny document-directed network access, filesystem reads, navigation away from the current document, and form submission. Keep subdocuments and scripts outside the authoring contract. Check actual resource entry points for CSS imports and URLs, images, fonts, and SVG references. Inline SVG may reference fragments in the current SVG. Embedded PNG decoding must validate byte and pixel limits. System font discovery remains an application operation independent of document URLs. A resource outside the contract can be skipped, while exceeding an enforced budget reports the limit. [Blitz document configuration](https://docs.rs/blitz-dom/latest/blitz_dom/struct.DocumentConfig.html)

| Budget | Provisional v0 limit | Enforcement point |
|---|---:|---|
| HTML input | 4 MiB | Read at most the limit plus one byte. |
| Parsed document | 40,000 nodes and depth 128 | Check before layout and semantic walks. Enforce asset budgets at the loader even if parsing triggers a load. |
| Embedded PNGs | 8 million pixels per image and 16 million pixels total | Validate dimensions with checked arithmetic before decode. Count each actual decode against the document budget. |
| Viewport raster | 8,388,608 pixels | Validate physical width, height, and byte count before buffer allocation. |
| Encoded PNG frame | 4 MiB | Bound the encoder output before base64 and Kitty framing. |

Treat these caps as starting values. Use the prototype to check they fit the reference report and the tested display sizes. Keep a single implementation of the resource policy so file input, stdin, graphics, and text obey the same rules.

### Verification and release evidence

Release scope, packaged artifact hashes, reproduced CLI/PTY/live-Ghostty
measurements, and unresolved OS-level checks are recorded in `docs/release.md`.
The table below remains the release checklist.

| Check | Evidence |
|---|---|
| Reference report | Ghostty screenshots at a narrow and a wide width, the exact macOS/Ghostty versions and display scale, and a walkthrough of navigation, disclosure, search, resize, and shell restoration. |
| Output modes | CLI tests for file and stdin, redirected stdout, `--text`, invalid UTF-8, closed downstream pipe, missing file, and a TTY with unavailable graphics. Assert actual output, diagnostics, and status. |
| Layout and input | Exercise page boundaries, keyboard focus, a missing fragment, a match inside a closed disclosure, an empty query, and an input during resize. Test state and position independently of font rasterization. |
| Offline handling | Assert no external access through images, CSS imports, fonts, SVG references, or link activation. Exercise limit boundaries and control-character removal through the normal entry points. |
| Terminal behavior | Check probe timeout and reply parsing, payload framing, reuse of one image ID, and cleanup after quit, interrupt, and handled errors. Use a pseudo-terminal for deterministic checks and Ghostty for visible placement and reflow. |
| Performance | Use a release build and record host, fixture, viewport, and sample count. Collect startup-to-first-frame-write, key-to-frame-write, peak process memory, and binary size. Compare the reference report with a long variant at the same viewport to distinguish DOM growth from raster growth. Note visual responsiveness separately because a frame write is a proxy for display. |

The renderer spike is the decision point for the pinned dependency set, viewport scaling, and the default authoring patterns. Record measured results and resolve any missing core behavior before the rest of the viewer is built around that engine revision. The v0 release includes the binary, example document, authoring guide, and the results of the release checks.

## Feasibility verdict

`kittyhtml` demonstrates HTML-to-image terminal output. Ghostty 1.4 provides the Kitty graphics primitives needed for the v0 pager. The live spike will establish whether the chosen Blitz build handles the reference report's layout and interaction at acceptable speed. [kittyhtml](https://github.com/kkukshtel/kittyhtml) · [Ghostty 1.4 release notes](https://ghostty.org/docs/install/release-notes/1-4-0)

## Target protocol facts

### Kitty graphics basics

- Transmit PNG with `f=100`, action `a=T`, suppressed responses `q=2`.
- Base64 payload, chunked into `<=4096` byte chunks; `m=1` for all but last.
- Terminal cell size: `TIOCGWINSZ` or `CSI 16t`; fallback assumptions are fragile.
- Kitty image storage quota is 320 MB per buffer; Ghostty has its own limits. Old images may be evicted.

### Two display modes

| Mode | How | Pros | Cons |
|---|---|---|---|
| **Direct placement** (`a=T`) | Image anchored to the current cell, image ID assigned | Simple; fewest escape sequences; Ghostty 1.4 scrolls it with text | Multiplexers/host apps do not understand it; redraw/reflow can misplace it |
| **Unicode placeholders** (`U=1`) | Emit U+10EEEE cells with row/column diacritics; image ID in foreground color | Treated as text; survives tmux/host redraw; scrollback-aware | PUA characters pollute copy/selection; more code; placeholder grid can reflow badly |

Direct placement is the v0 graphics path. Research for future tmux support points to Unicode placeholders with passthrough enabled.

### Ghostty 1.4 specifics

- 100% Kitty graphics conformance, including Unicode placeholders and relative placements.
- Images scroll with text inside scroll regions.
- Transmitting an existing image ID replaces the old image.
- Kitty graphics payloads capped at **65 MiB**.
- Zlib-compressed image data is supported.
- Unicode placeholder rendering is much cheaper than in 1.3.

### Detection

#### Environment hints

- `TERM_PROGRAM=ghostty`
- `TERM=xterm-ghostty`
- `KITTY_WINDOW_ID` / `TERM=xterm-kitty`
- `TERM_PROGRAM=WezTerm`

#### Protocol queries

- Use a basic graphics query to check support, with a deadline on terminal replies.
- A separate feature query with `a=q,i=<id>,e=T,U=1` can check for Unicode placeholders when needed. A supporting reply contains `U=1`. [Kitty capability queries](https://sw.kovidgoyal.net/kitty/graphics-protocol/#querying-support-for-protocol-features)

#### Inside tmux

- Ask the outer terminal: `tmux display -p '#{client_termname}'`.
- Use DCS passthrough and Unicode placeholders.

## Rendering options

| Option | Fidelity | Weight / latency | Verdict |
|---|---|---|---|
| **Blitz + Vello-CPU** | Embedded HTML/CSS with primitive DOM events. Flex, grid, and partial table support. | Build size and interactive latency to be measured with Glisk's reference report. | **Selected for v0.** |
| **Headless Chrome** | Full browser, CSS, JS, arbitrary sites. | System Chrome. Measured locally: **~0.49–0.55 s warm**, **~1.23 s cold** for a trivial 900×700 page. | Reference for future scripted documents. |
| **Native TUI browser** | CSS/HTML rendered by a terminal browser engine; interactive. | Medium to heavy. | Different product class; reference only. |
| **HTML → ANSI text** | No real CSS layout; text remains selectable, searchable, copyable. | Cheap. | Fallback / `--text` mode. |

### Blitz status notes

Supported / usable:

- Flexbox, grid, border-radius, box-shadow, background images, web fonts.
- Basic tables, lists, inline SVG rendering (styling SVGs is limited).
- The current feature matrix lists links and `<details>/<summary>` as supported. Confirm in the pinned build with terminal input. [Blitz element status](https://blitz.is/status/elements)

Known gaps:

- No JavaScript.
- Table layout is emulated with CSS Grid; border-collapse is partial.
- `overflow: auto` unsupported; `position: sticky` in progress.
- Fixed widths larger than the viewport overflow the canvas.
- `<thead>` content may paint empty in the version used by `kittyhtml`.
- RTL/inline text direction is partial; CJK fallback needs care.
- `break-inside` / page-break properties unsupported.

Conclusion: excellent for controlled/generated HTML. Use Chrome when the input is an arbitrary web page.

## Hard constraints and risks

| Risk | Impact | Mitigation |
|---|---|---|
| Rasterized text | Terminal selection and screen readers operate on terminal text | Search the document in the pager and provide `--text` for reading and copying |
| Resize and high-DPI geometry | Images may clip or use unreadably small text | Test cell fit and font size on the first target and reflow on resize |
| Long documents | Full-document rasterization raises memory and graphics payload size | Keep rasterization viewport-sized and replace the active image |
| Terminal and multiplexer differences | Protocol support and settings vary by host | Verify Ghostty first and fall back to text where graphics cannot be confirmed |
| CSS and SVG gaps | Some authored layouts and charts paint incompletely | Pin the renderer and specify the patterns verified by the reference report |
| Untrusted HTML | Secondary file and network access, expensive image decodes, control codes in text | Enforce the document resource policy and output limits in both modes |

## Similar tools

**Listed only. Nothing was cloned, downloaded, installed, or executed.**

### Closest architecture: HTML → image → terminal

| Tool | What it does | Notes |
|---|---|---|
| [kittyhtml](https://github.com/kkukshtel/kittyhtml) | HTML → Blitz → PNG → Kitty/iTerm2 inline image | Closest prior art. MIT. Young project (3★, 33 commits). Blitz 0.1 rc2 + Vello-CPU. Bundled Noto fonts. Known CSS limits. |
| [mcat](https://github.com/Skardyy/mcat) | Terminal image/video/PDF/Markdown viewer; can render HTML to image and display inline | Uses Chromium as optional dependency for HTML → image. Broader scope than Glisk; heavier. |
| [dmbdip](https://github.com/sapristi/dmbdip) | Markdown → image → Kitty graphics, including Mermaid diagrams | Adjacent: same render-to-image pattern, but Markdown input. |
| [terminal-browser](https://terminal-browser.com/) | Chromium-based browser rendered inside the terminal; tabs, mouse/keyboard, agent control | Interactive browser, not a one-shot inline block. Supports Ghostty/Kitty/tmux. |

### Native TUI browsers / HTML renderers

| Tool | What it does | Notes |
|---|---|---|
| [chawan](https://chawan.net/) | TUI web browser with its own HTML/CSS engine, optional JS, inline images via Kitty or Sixel | Interactive browser; supports CSS, flex, tables, images. Closest “real HTML in terminal” experience without screenshots. |
| [browsh](https://www.brow.sh/) | Firefox-backed text-based browser | Renders real pages to terminal cells; not pixel graphics. |
| [carbonyl](https://github.com/fathyb/carbonyl) | Chromium-backed text browser | Same class as Browsh; renders to colored text/half-blocks. |
| [w3m](https://w3m.sourceforge.net/) | Text browser; inline images via `w3mimgdisplay` | Text-first; image path is X11-era, not Kitty protocol. |
| [lynx](https://lynx.invisible-island.net/) | Classic text browser | Text only; useful fallback reference. |
| [links / elinks](http://links.twibright.com/) | Text browsers | Text only. |
| [offpunk](https://offpunk.net/) / `ansicat` | Offline-first CLI browser; `ansicat` renders HTML/Gemtext/images to terminal | HTML → ANSI, with optional Chafa images. Text selectable; lower fidelity. |

### HTML → text / ANSI

| Tool | What it does |
|---|---|
| [html2text](https://github.com/grobian/html2text) | HTML → plain text |
| [pandoc](https://pandoc.org/) | HTML → plain text / Markdown / other formats |
| [ansicat](https://sr.ht/~lioploum/offpunk/) | Standalone HTML/Gemtext/image renderer from the Offpunk project |
| [w3m -dump](https://w3m.sourceforge.net/) | HTML → text dump |

### Image display primitives

| Tool | What it does |
|---|---|
| `kitten icat` | Kitty’s reference image display kitten |
| `wezterm imgcat` | WezTerm inline image display |
| `iTerm2 imgcat` | iTerm2 inline image protocol |
| [chafa](https://github.com/hpjansson/chafa) | Image → ANSI/Kitty/Sixel; excellent fallback |
| [timg](https://github.com/hzeller/timg) | Terminal image/video viewer; Kitty/Sixel/ANSI |
| [viu](https://github.com/atanunq/viu) | Rust terminal image viewer; Kitty/iTerm2/Sixel |
| [catimg](https://github.com/posva/catimg) | Fast image → ANSI |
| [lsix](https://github.com/hackerb9/lsix) | Shell image viewer for Sixel terminals |
| [pixterm](https://github.com/eliukblau/pixterm) | Image → ANSI truecolor |
| [termpix](https://github.com/hopey-d/termpix) | Image → ANSI truecolor |
| [img2txt](https://github.com/cacalabs/libcaca) | libcaca image → ASCII/ANSI |
| [tpix](https://github.com/jesvedberg/tpix) | Static Kitty-protocol image viewer |
| [pixcat](https://github.com/mirukana/pixcat) | CLI/Python wrapper around Kitty graphics |
| [blackcat](https://github.com/j-c-m/blackcat) | `cat` with Kitty image support |
| [bicat](https://github.com/stevenxxiu/bicat) | Image viewer with nested tmux support |
| [kittyview](https://github.com/gominimal/kittyview) | Image/SVG viewer via Kitty protocol |
| [term-image](https://github.com/AnonymouX47/term-image) | Python CLI/TUI image browser |
| [vat](https://github.com/jzbrooks/vat) | Vector graphics (SVG/AVD) in terminal |
| [ueberzugpp](https://github.com/jstkdng/ueberzugpp) | Overlay image previews (X11/Wayland; not Kitty protocol) |

### Markdown-terminal analogues

| Tool | What it does | Relevance |
|---|---|---|
| [glow](https://github.com/charmbracelet/glow) | Markdown → styled ANSI terminal output | The UX reference; text, not images |
| [mdcat](https://github.com/swsnr/mdcat) | Markdown → terminal with images | Text + Kitty images |
| [mdfried](https://github.com/benjajaja/mdfried) | Markdown viewer with Kitty images | Adjacent |
| [kmv](https://github.com/parf/Kitty-Markdown-Viewer) | Kitty Markdown viewer | Adjacent |
| [presenterm](https://github.com/mfontanini/presenterm) | Markdown slides with Kitty/iTerm2/Sixel images | Shows terminal-image UX in a TUI |
| [mcat](https://github.com/Skardyy/mcat) | Multi-format viewer including Markdown/HTML | Already listed above |

### HTML/PNG renderers and converters (not terminal-specific)

| Tool | Role |
|---|---|
| [Blitz](https://github.com/DioxusLabs/blitz) | Rust HTML/CSS renderer; no JS; modular |
| [Servo](https://servo.org/) | Experimental browser engine; much heavier |
| Chrome headless | Full-fidelity screenshot path; `--headless --screenshot` |
| [Playwright](https://playwright.dev/) / [Puppeteer](https://pptr.dev/) | Browser automation/screenshot APIs |
| [WeasyPrint](https://weasyprint.org/) | HTML/CSS → PDF/PNG; print CSS, no JS |
| [wkhtmltoimage](https://wkhtmltopdf.org/) | Legacy Qt WebKit HTML → image |
| [html2png](https://github.com/Youpen-y/html2png) / [html3png](https://github.com/TaKO8Ki/html3png) | Standalone HTML → PNG converters |

## Prior art: `kittyhtml` details

Useful reference implementation:

- Pipeline: HTML string → Blitz DOM → layout → Vello-CPU raster → PNG → base64 → Kitty/iTerm2 escape.
- Bundled Noto Sans / Noto Sans Mono so output is deterministic across machines.
- PNG path: `f=100`; chunks `<=4096` base64 bytes.
- Direct Kitty placement outside tmux.
- Inside tmux: transmit-only plus virtual placements (`U=1`) and U+10EEEE placeholder rows, with image ID in foreground and placement ID in underline.
- Tall images split into bands because the row-diacritic table is finite.
- Known hacks: stdout gag around Blitz net logging; alpha-scan crop for auto-height.
- Build: ~500 crates, 3–4 minutes cold, ~15 MB stripped binary.
- MIT license.

This is enough to validate the approach; Glisk should not blindly fork it. Reimplement the encoder and choose our own renderer/CLI defaults.

## Name note

`glisk` is a Scots word for a gleam or glimpse. It fits the visual/terminal theme.

Availability checked during research:

- npm: free
- crates.io: free
- GitHub user handle: taken; use an org/repo name
- `glisk.dev`: taken
- Unrelated existing uses: Glisk Studio, a poker study tool

Confirm trademark/branding before publishing.

## Sources

- [Kitty terminal graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [Ghostty 1.4.0 release notes](https://ghostty.org/docs/install/release-notes/1-4-0)
- [Ghostty features](https://ghostty.org/docs/features)
- [kittyhtml](https://github.com/kkukshtel/kittyhtml)
- [Blitz](https://github.com/DioxusLabs/blitz)
- [Blitz CSS status](https://blitz.is/status/css)
- [Chawan](https://chawan.net/)
- [mcat](https://github.com/Skardyy/mcat)
- [terminal-browser](https://terminal-browser.com/)
- [Browsh](https://www.brow.sh/)
- [Carbonyl](https://github.com/fathyb/carbonyl)
- [Offpunk](https://offpunk.net/)
- [glow](https://github.com/charmbracelet/glow)
