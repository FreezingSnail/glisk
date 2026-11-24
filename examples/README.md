# Glisk examples

Every example is one self-contained HTML file. There are no external
stylesheets, fonts, images, scripts, or network requests. Open one directly,
pipe it through stdin, or browse a directory with the local picker.

```sh
glisk examples/tour                 # picker rooted at the tour
glisk examples/tour/index.html      # rich report, watched by default
glisk examples/tour/index.html --text
cat examples/tour/index.html | glisk -
```

## The tour

`examples/tour/` is the small feature tour. Each file is deliberately short and
uses stable ids so it also works as a live-reload target.

| File | What it exercises |
|---|---|
| `tour/index.html` | Responsive flex cards, headered table, inline SVG chart, fragment links, disclosures, searchable prose, narrow layout, stable ids. |
| `tour/motion.html` | CSS keyframes, pause/resume with `p`, `--pause-animation`, reduced-motion fallback, searchable caption. |
| `tour/reload.html` | Stable section and disclosure ids for save/atomic-replace reload; a working copy to edit while Glisk is open. |

Try the workflows the README visuals show:

```sh
glisk examples/tour                 # filter with "motion", Enter to open
glisk examples/tour/index.html      # "/" search, Tab+Enter a fragment, p for motion
cp examples/tour/reload.html reload-demo.html
glisk reload-demo.html              # edit reload-demo.html and save
glisk reload-demo.html --no-watch   # same file, snapshot session
```

The picker is text-only by design. `examples/tour` gives it three reports so
filtering, selection, Enter, and Escape are easy to see.

## Reference reports

- `decision-report.html` — the long reference report for the renderer,
  navigation, search, table, chart, and release evidence.
- `animated-cache.html` — a compact animation used by the authoring guide and
  the renderer timeline.
- `offline-denied.html` — a negative fixture that references external
  resources on purpose. Glisk must keep the readable body and skip every denied
  resource. It is not part of the tour and is not packaged.

See [`docs/authoring.md`](../docs/authoring.md) for the verified HTML subset,
budgets, reload ids, and the reader contract.
