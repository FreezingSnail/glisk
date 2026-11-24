# Keyboard navigation, fragment jumps, and disclosures

Bead: `glisk-2hp.5` · module: `src/navigation.rs` · tests:
`tests/navigation.rs` · spec:
`FEASIBILITY.md#navigation-and-search`

The module makes native fragment links and disclosure summaries operable in the
terminal. It enumerates eligible visible controls in document order, moves focus
on Tab and Shift+Tab, follows a decoded same-document fragment on Enter, toggles
the focused disclosure, and reports an unknown fragment without changing the
view. Focus and disclosure state stay on the live Blitz document, so a viewport
resize and reflow keep both useful.

The module is wired into the pager by bead `glisk-2hp.8`: `src/main.rs`
maps Tab/Shift+Tab/Enter to the [`Navigation`] methods. The integration test
still includes both modules with `#[path = "../src/viewport.rs"]` and
`#[path = "../src/navigation.rs"]`.

## Contract

```rust
let mut nav = navigation::Navigation::new();

// Tab / Shift+Tab
nav.focus_next(&mut viewport);
nav.focus_prev(&mut viewport);
let indicator = nav.focus_indicator(&mut viewport);

// Enter
nav.activate(&mut viewport);

// Search or pager reveal, without moving focus
navigation::show_fragment(&mut viewport, "#target%20id");
navigation::reveal_node(&mut viewport, matched_block_node)?;
```

| Item | Contract |
|---|---|
| `Navigation::focus_next` / `focus_prev` | Move DOM focus to the next or previous eligible control, scroll it into view, and return it. Traversal wraps; with no eligible focus, Tab starts at the first control and Shift+Tab at the last. |
| `Navigation::focused_control` | The focused control when DOM focus currently sits on an eligible visible one. |
| `Navigation::focus_indicator` | App-owned `FocusIndicator`: node, kind, document-relative `Rect`, and whether the rect intersects the current viewport. Use it for a status-row cue or overlay. |
| `Navigation::activate` | Enter. A fragment link calls `show_fragment`; a summary toggles its enclosing `<details>`. Returns the new `NavigationStatus`. |
| `Navigation::jump_to_fragment` / `reveal` | Status-tracking wrappers for the pager and later search. `reveal` does not change focus. |
| `navigation::controls(&BaseDocument)` | Eligible controls in document order. Callers must resolve layout first; the methods above do. |
| `navigation::show_fragment(&mut Viewport, fragment)` | Decode the fragment, find the exact `id`, open hiding disclosures, and scroll the target to the viewport top. Unknown ids return `UnknownFragment` before any scroll or mutation. |
| `navigation::reveal_node(&mut Viewport, node)` | Shared search hook: open closed disclosures that hide the node, resolve layout once, and align the node. Returns the node, opened disclosures, scroll offset, and whether alignment happened. |
| `navigation::decode_fragment` | `%XX` UTF-8 decoding used for exact `id` matching. |

`NavigationStatus` is `Idle`, `Focused`, `Jumped`, `Toggled`,
`UnknownFragment`, `NoFocus`, or `NotLocalLink`, with a `Display` message for the
status row.

## Eligible controls

| Control | Included when |
|---|---|
| Fragment link | `<a>` whose `href` starts with `#`. The raw fragment keeps its escapes; activation decodes it. |
| Disclosure summary | The first `<summary>` child of a `<details>`. |
| Hidden | Excluded by `hidden`, `aria-hidden="true"`, inline `display:none` / `visibility:hidden|collapse`, or the same computed style on any ancestor. |
| Closed disclosure | Controls in the body of a closed `<details>` are excluded; its summary remains eligible. Opening a disclosure re-enumerates its body on the next traversal. |
| Non-content | `head`, `script`, `style`, `template`, `noscript`, frames, and embedded/object content are skipped. |

Document order is the DOM order, not the CSS visual order. Enumeration is
recomputed from the live DOM on each traversal or activation, so opening or
closing a disclosure immediately changes the Tab order.

## Fragment jumps and reveal

1. Strip one leading `#` and percent-decode the fragment as UTF-8.
2. Find the first element whose `id` exactly equals the decoded string.
3. If absent, return `UnknownFragment` without scrolling, toggling, or changing
   focus. The status names the decoded fragment.
4. Otherwise open every closed `<details>` ancestor that hides the target,
   outermost first, and leave summaries that already contain it closed.
5. Resolve layout once and scroll through `Viewport::scroll_to`, so the
   viewport's `scroll_y` and the document's viewport scroll stay in sync.

`reveal_node` is the same operation for a known `NodeId`; the search bead calls
it for a matched block inside a closed disclosure. It does not move focus.

## Focus and resize

Focus is set with `BaseDocument::set_focus_to`, so it survives
`Viewport::resize` exactly as the live DOM does. Disclosure `open` attributes
also remain on the DOM. After reflow, `focused_control` and `focus_indicator`
derive their answer from the same document; the next Tab continues from the
retained focus. `Navigation` itself holds only the last status.

## Limits

- Only `<a>` fragment links are enumerated. `<area>`, buttons, forms, and
  general URL browsing are out of scope; non-fragment links are ignored and
  never navigated to.
- An empty fragment (`href="#"`) is treated as an unknown fragment. There is no
  `top` alias.
- Visibility is style- and disclosure-based. Overlapping or clipped controls,
  off-screen horizontal content, and paint-level occlusion are not modeled.
  Focus reveal adjusts vertical scroll only.
- Malformed percent escapes are copied literally; invalid UTF-8 decodes with
  replacement characters, matching browser URL behavior. The first element with
  a matching id wins.
- The focus indicator is data only. Wiring decides how to draw the cue; this
  module does not paint a focus ring or reserve a status row.
- Search, match counts, and `n`/`N` belong to `glisk-2hp.7`; this module supplies
  only traversal, activation, and the reveal hook.
