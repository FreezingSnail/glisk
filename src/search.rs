//! Pager search prompt, match counts, and next/previous match navigation.
//!
//! Bead: `glisk-2hp.7` · spec: `FEASIBILITY.md#navigation-and-search` ·
//! contract: semantic blocks from [`crate::text`] over one live
//! [`Viewport`](crate::viewport::Viewport).
//!
//! The pager owns the search prompt and the committed query. A query is matched
//! case-insensitively against [`TextBlock::searchable_text`] blocks, so a match
//! can span inline styling boundaries and text inside a closed disclosure still
//! participates. Matches are stored as the blocks' stable [`NodeId`]s; `n` and
//! `N` reuse [`Navigation::reveal`](crate::navigation::Navigation::reveal) so a
//! jump opens hiding disclosures and aligns the matched block.
//!
//! Prompt editing never touches the committed query. Escape clears the prompt
//! and restores the previous status, which keeps the prior query and match
//! position. Resize does not rebuild the DOM, so the query and match list stay
//! valid across [`Viewport::resize`](crate::viewport::Viewport::resize).
//!
//! Integration note: the wiring bead adds `mod search;` next to the existing
//! modules and maps `/`, Enter, Escape, `n`, and `N` to these methods. The
//! integration test includes this file with `#[path = "../src/search.rs"]`
//! together with the text, navigation, and viewport modules.

use blitz_dom::NodeId;

use crate::navigation::{Navigation, RevealOutcome};
use crate::text::DocumentText;
use crate::viewport::Viewport;

/// Status-row state for the search prompt or the active match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SearchStatus {
    /// No query, no prompt: the pager shows its normal status.
    #[default]
    Idle,
    /// The prompt is open; `text` is the current edit buffer.
    Prompt { text: String },
    /// Enter committed an empty query, so nothing is active.
    EmptyQuery,
    /// The committed query matched no semantic block.
    NoMatches { query: String },
    /// The committed query matched; `current` is one-based for display.
    Match {
        query: String,
        current: usize,
        total: usize,
    },
}

impl std::fmt::Display for SearchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => f.write_str("ready"),
            Self::Prompt { text } => write!(f, "/{text}"),
            Self::EmptyQuery => f.write_str("search cleared"),
            Self::NoMatches { query } => write!(f, "no matches: {query}"),
            Self::Match {
                query,
                current,
                total,
            } => write!(f, "\"{query}\" {current}/{total}"),
        }
    }
}

/// Pager search state: prompt, committed query, matches, and current position.
///
/// The struct never owns the document. Callers pass the [`DocumentText`]
/// extracted from the live viewport when committing a query, and the same
/// viewport plus [`Navigation`] when moving to a match.
#[derive(Clone, Debug, Default)]
pub struct Search {
    query: String,
    matches: Vec<NodeId>,
    current: Option<usize>,
    status: SearchStatus,
    editing: bool,
    prompt: String,
    prior_status: Option<SearchStatus>,
}

impl Search {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the prompt is open for editing.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Current prompt edit buffer. Empty when the prompt is closed.
    pub fn prompt_text(&self) -> &str {
        &self.prompt
    }

    /// The last committed query, preserved across Escape and resize.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Current status-row state.
    pub fn status(&self) -> &SearchStatus {
        &self.status
    }

    /// Number of semantic blocks matching the committed query.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Matched block nodes in document order.
    pub fn match_nodes(&self) -> &[NodeId] {
        &self.matches
    }

    /// Zero-based index of the current match.
    pub fn current_index(&self) -> Option<usize> {
        self.current.filter(|index| *index < self.matches.len())
    }

    /// One-based `(current, total)` for display, or `None` without a match.
    pub fn position(&self) -> Option<(usize, usize)> {
        let index = self.current_index()?;
        Some((index + 1, self.matches.len()))
    }

    /// Node of the current match, if one is selected.
    pub fn current_match(&self) -> Option<NodeId> {
        self.current_index().map(|index| self.matches[index])
    }

    /// Open the prompt with an empty edit buffer.
    ///
    /// The committed query, matches, and current position are untouched until
    /// [`Search::commit`] runs. Calling this while already editing only clears
    /// the edit buffer.
    pub fn begin(&mut self) {
        if !self.editing {
            self.prior_status = Some(self.status.clone());
        }
        self.editing = true;
        self.prompt.clear();
        self.status = SearchStatus::Prompt {
            text: String::new(),
        };
    }

    /// Append one character to the prompt buffer. Control characters are
    /// ignored, so Enter, Escape, and terminal keys stay with the pager.
    pub fn push_char(&mut self, ch: char) -> bool {
        if !self.editing || ch.is_control() {
            return false;
        }
        self.prompt.push(ch);
        self.refresh_prompt_status();
        true
    }

    /// Remove the last character from the prompt buffer.
    pub fn backspace(&mut self) -> bool {
        if !self.editing || self.prompt.pop().is_none() {
            return false;
        }
        self.refresh_prompt_status();
        true
    }

    /// Replace the whole prompt buffer, for paste or line editing.
    pub fn set_prompt(&mut self, text: impl Into<String>) -> bool {
        if !self.editing {
            return false;
        }
        self.prompt = text.into().chars().filter(|ch| !ch.is_control()).collect();
        self.refresh_prompt_status();
        true
    }

    /// Enter: commit the prompt as the query, select the first match, and
    /// reveal it. An empty query clears the active search and stays usable.
    pub fn commit(
        &mut self,
        text: &DocumentText,
        viewport: &mut Viewport,
        navigation: &mut Navigation,
    ) -> SearchStatus {
        let normalized = normalize_prompt(&self.prompt);
        self.editing = false;
        self.prior_status = None;
        self.query = normalized;
        self.prompt.clone_from(&self.query);
        self.matches.clear();
        self.current = None;

        if self.query.is_empty() {
            self.status = SearchStatus::EmptyQuery;
            return self.status.clone();
        }

        self.matches = text
            .matching_blocks(&self.query)
            .map(|block| block.node)
            .collect();

        if self.matches.is_empty() {
            self.status = SearchStatus::NoMatches {
                query: self.query.clone(),
            };
            return self.status.clone();
        }

        self.current = Some(0);
        self.update_match_status();
        let _ = navigation.reveal(viewport, self.matches[0]);
        self.status.clone()
    }

    /// Rebuild matches after the viewport swapped to a reloaded document.
    ///
    /// The committed query is preserved. When `previous_author` names the
    /// previous current match's author id and that id still anchors a new
    /// match, the same document location stays selected. Otherwise the old
    /// ordinal is clamped to the new match list. This never reveals or scrolls,
    /// so the restored reading section stays put.
    pub fn remap(
        &mut self,
        text: &DocumentText,
        viewport: &mut Viewport,
        previous_author: Option<String>,
    ) {
        if self.query.is_empty() {
            self.matches.clear();
            self.current = None;
            return;
        }
        let previous = self.current;
        self.matches = text
            .matching_blocks(&self.query)
            .map(|block| block.node)
            .collect();
        if self.matches.is_empty() {
            self.current = None;
            if !self.editing {
                self.status = SearchStatus::NoMatches {
                    query: self.query.clone(),
                };
            }
            return;
        }
        let mapped = previous_author.and_then(|author| {
            self.matches.iter().position(|node| {
                crate::viewport::author_id(viewport.document(), *node).as_deref()
                    == Some(author.as_str())
            })
        });
        let index = mapped
            .or_else(|| previous.map(|index| index.min(self.matches.len() - 1)))
            .unwrap_or(0);
        self.current = Some(index);
        if !self.editing {
            self.update_match_status();
        }
    }

    /// Escape: close the prompt, discard the edit buffer, and retain the prior
    /// query, matches, current position, and status.
    pub fn cancel(&mut self) -> SearchStatus {
        if !self.editing {
            return self.status.clone();
        }
        self.editing = false;
        self.prompt.clear();
        self.status = self.prior_status.take().unwrap_or_default();
        self.status.clone()
    }

    /// `n`: select and reveal the next match, wrapping from last to first.
    pub fn next(
        &mut self,
        viewport: &mut Viewport,
        navigation: &mut Navigation,
    ) -> Option<RevealOutcome> {
        self.step(viewport, navigation, 1)
    }

    /// `N`: select and reveal the previous match, wrapping from first to last.
    pub fn previous(
        &mut self,
        viewport: &mut Viewport,
        navigation: &mut Navigation,
    ) -> Option<RevealOutcome> {
        self.step(viewport, navigation, -1)
    }

    /// Reveal the current match again without changing its position. Useful
    /// after a resize when the caller wants to re-align the match.
    pub fn reveal_current(
        &mut self,
        viewport: &mut Viewport,
        navigation: &mut Navigation,
    ) -> Option<RevealOutcome> {
        let node = self.current_match()?;
        navigation.reveal(viewport, node).ok()
    }

    fn step(
        &mut self,
        viewport: &mut Viewport,
        navigation: &mut Navigation,
        direction: isize,
    ) -> Option<RevealOutcome> {
        if self.editing {
            return None;
        }
        if self.matches.is_empty() {
            self.current = None;
            self.status = if self.query.is_empty() {
                SearchStatus::Idle
            } else {
                SearchStatus::NoMatches {
                    query: self.query.clone(),
                }
            };
            return None;
        }

        let count = self.matches.len();
        let index = match self.current {
            Some(index) if index < count => {
                (index as isize + direction).rem_euclid(count as isize) as usize
            }
            _ if direction > 0 => 0,
            _ => count - 1,
        };
        self.current = Some(index);
        self.update_match_status();
        navigation.reveal(viewport, self.matches[index]).ok()
    }

    fn update_match_status(&mut self) {
        let Some(index) = self.current_index() else {
            self.status = SearchStatus::NoMatches {
                query: self.query.clone(),
            };
            return;
        };
        self.status = SearchStatus::Match {
            query: self.query.clone(),
            current: index + 1,
            total: self.matches.len(),
        };
    }

    fn refresh_prompt_status(&mut self) {
        self.status = SearchStatus::Prompt {
            text: self.prompt.clone(),
        };
    }
}

fn normalize_prompt(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_normalization_matches_extraction_whitespace() {
        assert_eq!(
            normalize_prompt("  alpha\t beta\n gamma "),
            "alpha beta gamma"
        );
        assert_eq!(normalize_prompt(" \t\n "), "");
    }
}
