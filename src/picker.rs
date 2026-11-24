//! Local report discovery and the keyboard picker state machine.
//!
//! Bead: `glisk-2hp.13`. Discovery reads filenames and metadata only; it never
//! opens a report. The picker keeps a bounded, newest-first list, filters by
//! relative path, and renders a plain-text screen sized to the terminal pane.
//! Opening a report is the caller's job: the picker returns a native path after
//! the caller rechecks it, so a removed or unreadable report leaves the list
//! usable.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Discovery budgets. A tree that exceeds any budget yields a partial list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_entries: usize,
    pub max_dirs: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 2_000,
            max_dirs: 4_096,
            max_depth: 24,
        }
    }
}

/// One regular `.htm`/`.html` report below the picker root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportEntry {
    /// Native path used for selection and ingestion.
    pub path: PathBuf,
    /// Root-relative path used for filtering and display.
    pub rel: String,
    pub modified: Option<SystemTime>,
}

/// Discovery result: sorted entries plus whether any budget was hit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub entries: Vec<ReportEntry>,
    pub partial: bool,
}

/// One discovery attempt, including a tree-walk failure on the root.
#[derive(Debug)]
pub struct Discovery {
    pub found: Found,
    pub error: Option<String>,
}

/// Find reports below `root`, newest modification first, then relative path.
///
/// Hidden entries and every symlink are skipped. Directories are only walked
/// within the depth budget, and both retained entries and visited directories
/// are bounded; exceeding a budget marks the result partial instead of failing.
#[cfg(test)]
pub fn discover(root: &Path) -> Discovery {
    discover_with_limits(root, Limits::default())
}

pub fn discover_with_limits(root: &Path, limits: Limits) -> Discovery {
    let mut found = Found::default();
    let mut error = None;
    let mut dirs = 0usize;
    // Iterative depth-first walk: (directory, relative path, depth).
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];

    while let Some((dir, rel, depth)) = stack.pop() {
        dirs += 1;
        if dirs > limits.max_dirs {
            found.partial = true;
            break;
        }
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(err) if rel.as_os_str().is_empty() => {
                error = Some(format!("cannot read {}: {err}", root.display()));
                break;
            }
            Err(_) => {
                // An unreadable nested directory is skipped; the rest of the
                // tree remains browsable.
                continue;
            }
        };
        for entry in read {
            let Ok(entry) = entry else {
                found.partial = true;
                continue;
            };
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let child_rel = rel.join(&name);
            if file_type.is_dir() {
                if depth + 1 > limits.max_depth {
                    found.partial = true;
                    continue;
                }
                stack.push((entry.path(), child_rel, depth + 1));
                continue;
            }
            if !file_type.is_file() || !is_report_name(&name) {
                continue;
            }
            if found.entries.len() >= limits.max_entries {
                found.partial = true;
                break;
            }
            let modified = entry.metadata().ok().and_then(|meta| meta.modified().ok());
            found.entries.push(ReportEntry {
                path: entry.path(),
                rel: path_text(&child_rel),
                modified,
            });
        }
        if found.partial {
            break;
        }
    }

    sort_entries(&mut found.entries);
    Discovery { found, error }
}

/// Newest modification first; relative path breaks ties. Entries without a
/// readable modification time sort last.
fn sort_entries(entries: &mut [ReportEntry]) {
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.rel.cmp(&b.rel)));
}

fn is_report_name(name: &std::ffi::OsStr) -> bool {
    let lower = name.to_string_lossy().to_ascii_lowercase();
    lower.ends_with(".htm") || lower.ends_with(".html")
}

fn path_text(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Keys the picker understands. The terminal parser maps raw bytes to these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Backspace,
    Char(char),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Changed,
    Open,
    Exit,
}

/// Picker list, filter, selection, and scroll state.
#[derive(Debug)]
pub struct Picker {
    root: PathBuf,
    entries: Vec<ReportEntry>,
    query: String,
    selected: usize,
    offset: usize,
    partial: bool,
    message: Option<String>,
    discovery_error: Option<String>,
    limits: Limits,
}

impl Picker {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_limits(root, Limits::default())
    }

    pub fn with_limits(root: impl Into<PathBuf>, limits: Limits) -> Self {
        Self {
            root: root.into(),
            entries: Vec::new(),
            query: String::new(),
            selected: 0,
            offset: 0,
            partial: false,
            message: None,
            discovery_error: None,
            limits,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn partial(&self) -> bool {
        self.partial
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// Discover again, keeping the filter and the selected path when both
    /// survive. Never discards the previous list on a failed tree walk.
    pub fn refresh(&mut self) {
        let selected = self.selected_path().map(Path::to_path_buf);
        match discover_with_limits(&self.root, self.limits) {
            Discovery {
                error: Some(error), ..
            } => {
                self.discovery_error = Some(error);
                self.partial = false;
            }
            Discovery { found, error: None } => {
                self.entries = found.entries;
                self.partial = found.partial;
                self.discovery_error = None;
                self.restore_selection(selected);
            }
        }
        self.clamp();
    }

    fn restore_selection(&mut self, selected: Option<PathBuf>) {
        if let Some(selected) = selected
            && let Some(index) = self
                .filtered()
                .iter()
                .position(|index| self.entries[*index].path == selected)
        {
            self.selected = index;
        } else {
            self.selected = 0;
        }
    }

    fn clamp(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.filtered_entry().map(|entry| entry.path.as_path())
    }

    fn filtered_entry(&self) -> Option<&ReportEntry> {
        let filtered = self.filtered();
        filtered
            .get(self.selected)
            .map(|index| &self.entries[*index])
    }

    fn filtered(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| query.is_empty() || entry.rel.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn handle(&mut self, key: Key, page: usize) -> Action {
        match key {
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-(page.max(1) as isize)),
            Key::PageDown => self.move_by(page.max(1) as isize),
            Key::Home => self.jump(0),
            Key::End => self.jump(usize::MAX),
            Key::Enter => {
                if self.selected_path().is_some() {
                    return Action::Open;
                }
                return Action::None;
            }
            Key::Escape => {
                if self.query.is_empty() {
                    return Action::Exit;
                }
                self.query.clear();
                self.selected = 0;
                self.offset = 0;
                self.message = None;
                return Action::Changed;
            }
            Key::Backspace => {
                if self.query.pop().is_some() {
                    self.selected = 0;
                    self.offset = 0;
                    self.message = None;
                    return Action::Changed;
                }
                return Action::None;
            }
            Key::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.selected = 0;
                self.offset = 0;
                self.message = None;
                return Action::Changed;
            }
            _ => return Action::None,
        }
    }

    fn move_by(&mut self, delta: isize) -> Action {
        let len = self.filtered().len();
        if len == 0 {
            return Action::None;
        }
        let next = (self.selected as isize + delta).clamp(0, len as isize - 1);
        if next as usize == self.selected {
            return Action::None;
        }
        self.selected = next as usize;
        self.message = None;
        Action::Changed
    }

    fn jump(&mut self, index: usize) -> Action {
        let len = self.filtered().len();
        if len == 0 {
            return Action::None;
        }
        let next = index.min(len - 1);
        if next == self.selected {
            return Action::None;
        }
        self.selected = next;
        self.message = None;
        Action::Changed
    }

    /// Plain-text screen sized to `cols` x `rows`. The caller owns terminal
    /// painting; this function only produces content.
    pub fn render(&mut self, cols: u16, rows: u16) -> Vec<String> {
        let cols = usize::from(cols.max(1));
        let rows = usize::from(rows.max(2));
        let filtered = self.filtered();
        let list_rows = rows.saturating_sub(4);
        if list_rows > 0 {
            self.ensure_visible(list_rows, filtered.len());
        }

        let mut lines = Vec::with_capacity(rows);
        let root = sanitize_text(&self.root.display().to_string());
        let count = if self.query.is_empty() {
            format!("{} report(s)", self.entries.len())
        } else {
            format!("{} match(es)", filtered.len())
        };
        let partial = if self.partial { ", partial" } else { "" };
        lines.push(fit(
            &format!("Glisk reports · {root} · {count}{partial}"),
            cols,
        ));

        let query = if self.query.is_empty() {
            "type to filter by relative path".to_string()
        } else {
            format!("filter: {}", sanitize_text(&self.query))
        };
        lines.push(fit(&format!(" {query}"), cols));

        if list_rows == 0 {
            return lines;
        }

        if filtered.is_empty() {
            let empty = if self.entries.is_empty() {
                if let Some(error) = &self.discovery_error {
                    format!(" {error}")
                } else {
                    " No reports found. Pass a report path to open one directly.".to_string()
                }
            } else {
                format!(" No reports match “{}”. Esc clears the filter.", self.query)
            };
            lines.push(fit(&empty, cols));
            for _ in 1..list_rows {
                lines.push(String::new());
            }
        } else {
            for row in 0..list_rows {
                let index = self.offset + row;
                let Some(&entry_index) = filtered.get(index) else {
                    lines.push(String::new());
                    continue;
                };
                let entry = &self.entries[entry_index];
                let marker = if index == self.selected { "> " } else { "  " };
                lines.push(fit(&format!("{marker}{}", sanitize_text(&entry.rel)), cols));
            }
        }

        let status = self
            .message
            .as_deref()
            .or(self.discovery_error.as_deref())
            .map(sanitize_text)
            .unwrap_or_default();
        lines.push(fit(&format!(" {status}"), cols));
        lines.push(fit(
            " Up/Down move  PgUp/PgDn page  Enter open  Esc clear/exit  Ctrl+C quit",
            cols,
        ));
        lines.truncate(rows);
        lines
    }

    fn ensure_visible(&mut self, height: usize, len: usize) {
        if len == 0 {
            self.offset = 0;
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        self.offset = self.offset.min(len.saturating_sub(height));
    }
}

/// Strip escape/control characters and turn tabs into single spaces. Native
/// paths stay untouched; this is display text only.
pub fn sanitize_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Drop a complete CSI/OSC/DCS introducer and its payload.
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('_') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if ch == '\t' {
            out.push(' ');
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

/// Terminal cell width used for truncation. Zero-width marks and joiners do not
/// advance; East Asian wide/fullwidth and emoji count as two cells.
fn char_cells(ch: char) -> usize {
    let code = ch as u32;
    if ch.is_control() || code == 0x200d || (0xfe00..=0xfe0f).contains(&code) {
        return 0;
    }
    if (0x0300..=0x036f).contains(&code)
        || (0x1ab0..=0x1aff).contains(&code)
        || (0x1dc0..=0x1dff).contains(&code)
        || (0x20d0..=0x20ff).contains(&code)
        || (0xfe20..=0xfe2f).contains(&code)
    {
        return 0;
    }
    if (0x1100..=0x115f).contains(&code)
        || (0x2e80..=0x303e).contains(&code)
        || (0x3041..=0x33ff).contains(&code)
        || (0x3400..=0x4dbf).contains(&code)
        || (0x4e00..=0x9fff).contains(&code)
        || (0xa000..=0xa4cf).contains(&code)
        || (0xa960..=0xa97f).contains(&code)
        || (0xac00..=0xd7a3).contains(&code)
        || (0xf900..=0xfaff).contains(&code)
        || (0xfe10..=0xfe19).contains(&code)
        || (0xfe30..=0xfe6f).contains(&code)
        || (0xff00..=0xff60).contains(&code)
        || (0xffe0..=0xffe6).contains(&code)
        || (0x1f300..=0x1faff).contains(&code)
        || (0x20000..=0x3fffd).contains(&code)
    {
        return 2;
    }
    1
}

fn fit(text: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    if text.chars().map(char_cells).sum::<usize>() <= cols {
        return text.to_string();
    }
    let mut width = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let cells = char_cells(ch);
        if width + cells > cols - 1 {
            break;
        }
        width += cells;
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(rel: &str, seconds: u64) -> ReportEntry {
        ReportEntry {
            path: PathBuf::from(rel),
            rel: rel.to_string(),
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
        }
    }

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/picker-tree")
    }

    #[test]
    fn discovery_finds_nested_reports_and_skips_hidden_and_symlinks() {
        let discovery = discover(&fixture_root());
        assert!(discovery.error.is_none(), "{:?}", discovery.error);
        let mut rels: Vec<&str> = discovery
            .found
            .entries
            .iter()
            .map(|entry| entry.rel.as_str())
            .collect();
        rels.sort_unstable();
        assert_eq!(
            rels,
            vec!["nested/child.html", "reports/UPPER.HTML", "top.htm"],
            "only visible regular reports are discovered"
        );
        assert!(!discovery.found.partial);
    }

    #[test]
    fn sort_is_newest_first_then_relative_path() {
        let mut found = Found {
            entries: vec![
                entry("b.html", 10),
                entry("a.html", 10),
                entry("new.html", 20),
            ],
            partial: false,
        };
        sort_entries(&mut found.entries);
        let rels: Vec<&str> = found.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["new.html", "a.html", "b.html"]);
    }

    #[test]
    fn missing_root_reports_an_error_without_entries() {
        let discovery = discover(Path::new("tests/fixtures/definitely-missing-picker-root"));
        assert!(discovery.found.entries.is_empty());
        assert!(discovery.error.is_some());
    }

    #[test]
    fn budgets_mark_partial_results() {
        let limits = Limits {
            max_entries: 1,
            max_dirs: 64,
            max_depth: 8,
        };
        let discovery = discover_with_limits(&fixture_root(), limits);
        assert!(discovery.found.partial);
        assert_eq!(discovery.found.entries.len(), 1);

        let limits = Limits {
            max_entries: 100,
            max_dirs: 1,
            max_depth: 8,
        };
        let discovery = discover_with_limits(&fixture_root(), limits);
        assert!(discovery.found.partial, "directory budget is enforced");
    }

    #[test]
    fn empty_directory_is_a_normal_empty_state() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/picker-empty");
        let discovery = discover(&root);
        assert!(discovery.error.is_none(), "{:?}", discovery.error);
        assert!(discovery.found.entries.is_empty());
        let mut picker = Picker::new(root);
        picker.refresh();
        let lines = picker.render(50, 6);
        assert!(lines[2].contains("No reports found"), "{lines:?}");
    }

    #[test]
    fn display_text_sanitizes_report_names() {
        let mut picker = Picker::new(".");
        picker.entries = vec![entry("bad\x1b[31m/na\tme.html", 1)];
        let lines = picker.render(60, 6);
        assert!(!lines[2].contains('\x1b'), "{lines:?}");
        assert!(!lines[2].contains('\t'), "{lines:?}");
        assert!(lines[2].contains("bad/na me.html"), "{lines:?}");
    }

    #[test]
    fn filter_is_case_insensitive_and_escape_clears_it() {
        let mut picker = Picker::new(".");
        picker.entries = vec![entry("alpha/report.HTML", 1), entry("beta/other.htm", 2)];
        assert_eq!(picker.handle(Key::Char('R'), 10), Action::Changed);
        assert_eq!(picker.handle(Key::Char('E'), 10), Action::Changed);
        assert_eq!(picker.selected_path(), Some(Path::new("alpha/report.HTML")));
        assert_eq!(picker.handle(Key::Escape, 10), Action::Changed);
        assert_eq!(picker.query(), "");
        assert_eq!(picker.handle(Key::Escape, 10), Action::Exit);
    }

    #[test]
    fn enter_opens_only_with_a_selection() {
        let mut picker = Picker::new(".");
        assert_eq!(picker.handle(Key::Enter, 10), Action::None);
        picker.entries = vec![entry("only.html", 1)];
        assert_eq!(picker.handle(Key::Enter, 10), Action::Open);
    }

    #[test]
    fn refresh_preserves_filter_and_selected_path() {
        let mut picker = Picker::new(fixture_root());
        picker.refresh();
        picker.query = "nested".to_string();
        picker.selected = 0;
        let selected = picker.selected_path().map(Path::to_path_buf);
        assert!(selected.is_some());
        picker.refresh();
        assert_eq!(picker.query(), "nested");
        assert_eq!(picker.selected_path(), selected.as_deref());
    }

    #[test]
    fn render_shows_query_selection_and_empty_states() {
        let mut picker = Picker::new("reports");
        assert!(picker.render(40, 6)[0].contains("0 report(s)"));
        assert!(picker.render(40, 6)[2].contains("No reports found"));

        picker.entries = vec![entry("a/one.html", 1), entry("b/two.html", 2)];
        picker.query = "zzz".to_string();
        let lines = picker.render(40, 6);
        assert!(lines[2].contains("No reports match"));

        picker.query = "two".to_string();
        let lines = picker.render(40, 6);
        assert!(lines[2].starts_with("> "), "{lines:?}");
        assert!(lines[2].contains("b/two.html"));
    }

    #[test]
    fn selection_stays_visible_when_scrolling() {
        let mut picker = Picker::new(".");
        picker.entries = (0..10).map(|i| entry(&format!("{i}.html"), i)).collect();
        for _ in 0..9 {
            picker.handle(Key::Down, 3);
        }
        let lines = picker.render(40, 7);
        assert!(
            lines[2..5].iter().any(|line| line.contains("> 9.html")),
            "{lines:?}"
        );
    }

    #[test]
    fn display_text_strips_controls_and_fits_cells() {
        assert_eq!(sanitize_text("a\x1b[31mb\tc\n"), "ab c");
        assert_eq!(fit("abcdef", 4), "abc…");
        // Two CJK glyphs (four cells) do not fit beside the ellipsis.
        assert_eq!(fit("报告x", 4), "报…");
    }
}
