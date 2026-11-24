# Authoring a Glisk report

Glisk v0 reads one local `.htm`/`.html` file (or stdin) and presents it as a
live terminal pager. Start from `examples/decision-report.html`; it is the
reference for every pattern below and is verified in Ghostty 1.4 on macOS.

```sh
glisk report.html          # live pager when the terminal passes the probe
glisk report.html --text   # plain UTF-8 for pipes, copying, or review
cat report.html | glisk -  # same document bytes from stdin
```

Glisk is a local document reader, not a browser. It never fetches the network,
reads secondary files, runs scripts, or navigates away from the supplied
document. Author a single self-contained file with inline CSS, inline SVG, and
bounded `data:image/png` assets. External stylesheets, fonts, images, iframes,
data URLs other than PNG, and non-fragment links are denied; the readable body
still renders.

## Supported patterns

Verified by the reference report and the renderer spike:

- Normal block flow, headings, paragraphs, lists, `pre`/`code`, quotes, and
  horizontal rules.
- Flexbox cards with `display:flex`, `flex-wrap:wrap`, `flex:1 1 <basis>`,
  `min-width:0`, and `gap`. This is the responsive layout to use.
- Solid colors, borders, `border-radius`, `box-shadow`, and background images.
- A headered table with `caption`, `thead`, `th[scope=col]`, and
  `table-layout:fixed`. Keep it to four columns or fewer and allow wrapping
  with `overflow-wrap:break-word`.
- Inline static SVG with `viewBox`, shapes, lines, text, and a `role="img"`
  label. Put an explicit `font-family` on SVG `<text>`; SVG text does not
  inherit the document font stack.
- Same-document `<a href="#id">` links with unique `id` targets.
- Native `<details><summary>` disclosures, optionally with `open`.
- A visible focus style such as `a:focus, summary:focus { outline: ... }`.
- `@media (max-width: 640px)` rules for the narrow viewport.

Avoid JavaScript, external resources, `overflow:auto` scrolling regions,
`position:sticky`, generated content that carries meaning, and fixed widths
larger than the viewport. They are outside the verified subset. Tables and
cards should wrap rather than force horizontal scrolling.

## Skeleton

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Decision report</title>
<style>
  :root { color-scheme: dark; --bg:#0f1419; --card:#1b2430; --line:#3b4d60;
          --ink:#e8eef5; --muted:#a9b8c8; --accent:#71d6a8; }
  * { box-sizing: border-box; }
  html { background: var(--bg); color: var(--ink); }
  body { margin:0; padding:24px 20px 40px; font-family:"Helvetica Neue",Helvetica,Arial,sans-serif;
         font-size:16px; line-height:1.45; }
  main { max-width:980px; margin:0 auto; }
  h1 { font-size:30px; margin:0 0 8px; }
  p { color: var(--muted); margin:0 0 10px; }
  .cards { display:flex; flex-wrap:wrap; gap:12px; }
  .card { flex:1 1 260px; min-width:0; background:var(--card);
          border:1px solid var(--line); border-radius:12px; padding:14px 15px; }
  a { color: var(--accent); }
  a:focus, summary:focus { outline:3px solid var(--accent); outline-offset:2px; }
  @media (max-width: 640px) { body { padding:16px 12px 32px; font-size:15px; } }
</style>
</head>
<body>
<main>
  <h1>Report title</h1>
  <p class="lede">One-sentence decision and the evidence behind it.</p>
  <nav aria-label="Sections">
    <a href="#summary">Summary</a>
    <a href="#evidence">Evidence</a>
  </nav>
  <section id="summary">
    <h2>Summary</h2>
    <div class="cards">
      <div class="card"><p>Decision</p></div>
      <div class="card"><p>Cost</p></div>
    </div>
  </section>
  <section id="evidence">
    <h2>Evidence</h2>
    <details id="method"><summary>Method</summary>
      <p>Searchable even while closed.</p>
    </details>
  </section>
</main>
</body>
</html>
```

## Reader contract

| Action | Result |
|---|---|
| Arrows, `j`/`k`, Page Up/Down | Scroll a step or page. |
| Tab, Shift+Tab | Move visible focus through fragment links and disclosure summaries. |
| Enter | Follow the focused fragment link or toggle its disclosure. |
| `/`, `n`, `N` | Search all semantic text and cycle matches; a match inside a closed disclosure opens it. |
| Resize | Reflow at the new width and keep the reading position, focus, and open disclosures. |
| `q`, Ctrl+C | Delete the image and restore the terminal. |

Give every important section a unique `id`, label every link with its
destination text, and put the conclusion in prose. The same semantic text is
used for search and `--text`; do not hide required information in color, shape,
or an image alone. For charts, include authored `<title>` and `<desc>` plus a
visible text summary. Use meaningful `alt` text on images.

## Budgets

| Limit | v0 value |
|---|---:|
| HTML input | 4 MiB |
| DOM nodes / depth | 40,000 / 128 |
| Embedded PNG | 8,000,000 pixels per image; 16,000,000 total |
| Viewport raster | 8,388,608 pixels |
| Encoded PNG frame | 4 MiB |

A document over the input or DOM budget fails with an error. A denied image is
skipped so the rest of the document stays readable; exceeding an enforced image
budget reports the limit. The viewport is rasterized one screen at a time, so a
long report does not allocate a document-sized image.

## Plain text and fallback

`--text`, redirected stdout, a missing TTY, unsupported graphics, missing cell
geometry, or a probe timeout produce plain UTF-8 text with no terminal control
sequences. The walk preserves headings (`#`), lists, table row/header order,
code indentation, disclosure bodies, captions, image descriptions, and SVG
summaries. CSS visual reordering is not simulated; document order wins.

## Check the result

```sh
# Text must be readable and contain no ESC bytes.
glisk examples/decision-report.html --text | grep -n '^#' 
glisk examples/decision-report.html --text | LC_ALL=C grep -q $'\x1b' || echo "plain: ok"

# Live narrow/wide frames in Ghostty (replace the size as needed).
glisk examples/decision-report.html
```

See `docs/cli.md` for the complete CLI and `docs/release.md` for the verified
environment, packaging procedure, and measured release evidence.
