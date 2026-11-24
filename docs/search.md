# Pager search, match counts, and next/previous matches

Bead: `glisk-2hp.7` · module: `src/search.rs` · tests:
`tests/search.rs` · spec: `FEASIBILITY.md#navigation-and-search`

The module adds the pager search prompt. It edits a query, commits it with
Enter, matches case-insensitively against the semantic blocks from
`src/text.rs`, reports the current match count, and moves through matches
with `n`/`N`, wrapping in both directions. Escape closes the prompt and keeps
the previous query and position. Matches inside closed disclosures are found
because extraction includes their bodies, and revealing a match uses the shared
`Navigation::reveal` hook, which opens hiding disclosures and aligns the block.

The module is wired into the pager by bead `glisk-2hp.8`: `src/main.rs`
maps `/`, Enter, Escape, `n`, and `N` to the [`Search`] methods. The integration
test still includes the text, navigation, viewport, and search modules with
`#[path = "..."]`.

## Contract

```rust
let mut search = search::Search::new();

// `/`
search.begin();
search.push_char('q');
search.backspace();
search.set_prompt("needle");       // paste / line replacement
let editing = search.prompt_text();

// Enter
let text = text::extract(viewport.document());
let status = search.commit(&text, &mut viewport, &mut navigation);

// Escape
search.begin();
search.push_char('x');
search.cancel();                   // previous query and matches survive

// `n` / `N`
search.next(&mut viewport, &mut navigation);
search.previous(&mut viewport, &mut navigation);

// State for the status row and tests
search.query();
search.match_count();
search.position();                 // Some((current_one_based, total))
search.current_match();
search.status();
```

| Item | Contract |
|---|---|
| `Search::begin` | Open the prompt with an empty edit buffer. The committed query, matches, and position are untouched. |
| `Search::push_char` / `backspace` / `set_prompt` | Edit the prompt only. Control characters are ignored, so Enter, Escape, and terminal keys stay with the pager. |
| `Search::commit` | Enter. Normalize the prompt's whitespace, match semantic blocks in document order, select the first match, and reveal it. An empty query clears the active search. |
| `Search::cancel` | Escape. Close the prompt, discard the edit buffer, and restore the previous query, matches, current position, and status. Cancel before any committed query returns to `Idle`. |
| `Search::next` / `previous` | `n` / `N`. Select and reveal the next or previous match, wrapping last→first and first→last. Ignored while the prompt is open. With no matches, keeps an explicit `NoMatches`/`Idle` status and returns `None`. |
| `Search::reveal_current` | Re-reveal the current match without changing its position, for example after a resize. Returns `None` when no match is selected or the node is gone. |
| `Search::query` / `prompt_text` | The committed query and the live edit buffer separately. |
| `Search::match_count` / `match_nodes` | Match count and matched block nodes in document order. |
| `Search::current_index` / `position` / `current_match` | Zero-based index, one-based `(current, total)`, and the current `NodeId`. |
| `Search::status` | `SearchStatus` with a `Display` message for the status row. |

`SearchStatus` is `Idle`, `Prompt { text }`, `EmptyQuery`,
`NoMatches { query }`, or `Match { query, current, total }`. `Match` stores the
one-based current position for direct display. The status row can print the
value with `Display`: `/needle`, `"needle" 2/3`, `no matches: needle`, or
`search cleared`.

## Prompt lifecycle

1. `/` calls `begin` and shows `Prompt { text: "" }`.
2. Typing calls `push_char`; Backspace calls `backspace`. Neither touches the
   committed query or match list.
3. Enter calls `commit` with the current `DocumentText`. Whitespace runs in the
   prompt collapse to single spaces, matching text extraction; the query keeps
   its letter case and matching is case-insensitive.
4. Escape calls `cancel` and restores the status saved when the prompt opened.
   A new search that is cancelled therefore leaves the previous query, the
   previous match list, and the previous current match intact.

## Matching and reveal

- Matching walks `DocumentText::matching_blocks`, which compares
  `TextBlock::search_text` case-insensitively. Inline descendants are
  concatenated before normalization, so `Search <strong>across</strong>
  <em>styled</em>` matches the query `across styled` as one block.
- Script, style, template, and explicitly hidden content never appear in the
  block list. Closed disclosure bodies do appear, so a query finds them before
  the reader opens the disclosure.
- Committing a non-empty query with matches selects the first block and calls
  `Navigation::reveal`, which opens every closed `<details>` ancestor that hides
  the match, resolves layout once, and scrolls the block into view.
- `n`/`N` preserve the same reveal path. The match list is the block list, not
  per-word occurrences: one matching paragraph counts once even if the query
  appears twice inside it.
- An empty committed query clears the query and matches and reports
  `EmptyQuery`. A non-empty query with no matches reports `NoMatches` and
  leaves scrolling, navigation, and a later search usable.

## Resize and state ownership

The module stores only the query, matched `NodeId`s, current index, prompt
buffer, and status. It never rebuilds the document or extracts inside a paint.
`Viewport::resize` reflows the same live DOM, so the query, block nodes, match
list, current position, and status remain valid; the caller can continue with
`n`/`N` or re-align with `reveal_current`.

DOM state such as focus and expanded disclosures stays on the `BaseDocument`
through the shared navigation and viewport modules. Search state stays in
`Search`, matching the pager ownership boundary in `FEASIBILITY.md`.

## Limits

- Match granularity is the semantic block. The module does not highlight the
  matched substring, count repeated occurrences inside one block, or provide
  per-line match rectangles.
- Matching is substring-based, case-insensitive, and whitespace-normalized. It
  does not implement regular expressions, word boundaries, fuzzy matching, or
  locale-specific case folding.
- Only the live document's semantic text is searched. Metadata, CSS content,
  `alt`-less images, and unavailable chart text are not searchable.
- If a matched node disappears between commit and `n`/`N`, the index still
  advances but reveal can fail; v0 does not remove document nodes after load.
  The prompt also has no cursor movement or command history.
- Search state is not painted here. Wiring maps `SearchStatus` and
  `Search::prompt_text` into the reserved search/status row.
