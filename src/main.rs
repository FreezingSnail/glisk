//! Glisk pager CLI.
//!
//! Reads one local HTML document (or stdin), applies the bounded ingestion and
//! offline resource policy, then either writes plain text or opens the live
//! Kitty viewport through the pager modules.
//!
//! The hidden `--snapshot`, `--self-test`, `--scripted-actions`, `--once`,
//! `--dump-dir`, and `--metrics` flags remain as diagnostic entry points for
//! the renderer evidence recorded in `docs/renderer-spike.md`.

mod document;
mod navigation;
mod picker;
mod search;
mod terminal;
mod text;
mod tmux;
mod viewport;
mod watch;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyrender_vello_cpu::VelloCpuScenePainter;
use blitz_dom::{BaseDocument, ScrollBehavior, ScrollLogicalPosition, local_name};
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use clap::Parser;

use document::{IngestError, IngestedDocument, Limits};
use navigation::Navigation;
use search::Search;
use terminal::{Geometry, TerminalConfig, TerminalError, TerminalOwner};
use viewport::{AuthorAnchor, PaintOutcome, ResizeOutcome, ViewGeometry, Viewport as DocViewport};

const MAX_RASTER_PIXELS: u64 = 8_388_608;
const TEXT_VIEWPORT_WIDTH: u32 = 420;
const TEXT_VIEWPORT_HEIGHT: u32 = 900;
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(100);
const SCROLL_STEP: f64 = 40.0;

#[derive(Parser, Debug)]
#[command(name = "glisk", about = "Glisk HTML pager", version)]
struct Args {
    /// Local .htm/.html file, `-` for stdin, or a directory to pick from.
    /// Omit it to pick reports below the current directory.
    input: Option<String>,

    /// Write plain UTF-8 text instead of opening the live pager.
    #[arg(long)]
    text: bool,

    /// Open the live pager with animations paused.
    #[arg(long)]
    pause_animation: bool,

    /// Open a named local report once; do not follow later file changes.
    #[arg(long)]
    no_watch: bool,

    /// Diagnostic: write one viewport PNG to this path.
    #[arg(long, hide = true)]
    snapshot: Option<String>,

    /// Diagnostic: CSS width for --snapshot.
    #[arg(long, default_value_t = 420.0, hide = true)]
    css_width: f64,

    /// Diagnostic: CSS height for --snapshot.
    #[arg(long, default_value_t = 900.0, hide = true)]
    css_height: f64,

    /// Diagnostic: device scale for --snapshot.
    #[arg(long, default_value_t = 2.0, hide = true)]
    scale: f64,

    /// Diagnostic: scroll to an element id before painting or opening.
    #[arg(long, hide = true)]
    fragment: Option<String>,

    /// Diagnostic: toggle a <details> element id before painting or opening.
    #[arg(long, hide = true)]
    toggle: Option<String>,

    /// Diagnostic: focus the first node matching this CSS selector.
    #[arg(long, hide = true)]
    focus: Option<String>,

    /// Diagnostic: draw once, wait briefly, then exit.
    #[arg(long, hide = true)]
    once: bool,

    /// Diagnostic: run a fixed action sequence through the live handlers.
    #[arg(long, hide = true)]
    scripted_actions: bool,

    /// Diagnostic: run headless interaction and resize assertions.
    #[arg(long, hide = true)]
    self_test: bool,

    /// Diagnostic: paint every page from top to bottom and record the largest
    /// raster and encoded frame (release memory evidence for long reports).
    #[arg(long, hide = true)]
    scroll_sweep: bool,

    /// Diagnostic: write every live PNG frame into this directory.
    #[arg(long, hide = true)]
    dump_dir: Option<String>,

    /// Diagnostic: write key=value metrics here when the process exits.
    #[arg(long, hide = true)]
    metrics: Option<String>,
}

fn main() {
    let started = Instant::now();
    let args = Args::parse();

    let result = if args.self_test {
        self_test(&args, started)
    } else if args.scroll_sweep {
        scroll_sweep(&args, started)
    } else if let Some(path) = &args.snapshot {
        snapshot(&args, path, started)
    } else {
        run_cli(&args, started)
    };

    if let Err(err) = result {
        eprintln!("glisk: {err}");
        std::process::exit(1);
    }
}

fn run_cli(args: &Args, started: Instant) -> Result<(), String> {
    match resolve_source(args)? {
        Source::Document(input) => run_document(args, &input, started),
        Source::Picker(root) => {
            // The picker is an interactive list only. Text mode needs a named
            // report (or stdin); there is no separate text picker.
            if args.text || !terminal::is_tty(libc::STDOUT_FILENO) {
                return Err(picker_needs_report_hint());
            }
            run_picker(args, root, started)
        }
    }
}

/// What the positional argument selected. A path that is not an existing
/// directory is treated as a document, so missing-file and URL diagnostics stay
/// identical to earlier releases.
enum Source {
    Document(String),
    Picker(PathBuf),
}

fn resolve_source(args: &Args) -> Result<Source, String> {
    match args.input.as_deref() {
        None => std::env::current_dir()
            .map(Source::Picker)
            .map_err(|err| format!("cannot read current directory: {err}")),
        Some("-") => Ok(Source::Document("-".to_string())),
        Some(input) if looks_like_url(input) => Ok(Source::Document(input.to_string())),
        Some(input) if Path::new(input).is_dir() => Ok(Source::Picker(PathBuf::from(input))),
        Some(input) => Ok(Source::Document(input.to_string())),
    }
}

fn picker_needs_report_hint() -> String {
    "no report selected; pass a report path for text output (for example: glisk report.html --text)"
        .to_string()
}

/// Hidden diagnostics operate on exactly one named document.
fn required_input(args: &Args) -> Result<&str, String> {
    args.input
        .as_deref()
        .ok_or_else(|| "this mode needs one report path".to_string())
}

/// Ingest and present one document: unchanged file, stdin, text, and watch
/// behavior.
fn run_document(args: &Args, input: &str, started: Instant) -> Result<(), String> {
    // Ingest with a deterministic text viewport first. The live path re-lays
    // out the same live document at terminal geometry after the owner opens.
    //
    // A possible live file session captures the source bytes and metadata
    // before ingestion, so the watcher can distinguish a real change from the
    // revision already on screen (including a change between ingest and the
    // first frame) and can skip a metadata-only touch with identical bytes.
    let (ingested, baseline) =
        if wants_watch(args, input) && !args.text && terminal::is_tty(libc::STDOUT_FILENO) {
            let state = watch::path_state(Path::new(input));
            if matches!(state, watch::PathState::File(_)) {
                let html = document::read_html_file(input, &Limits::default())
                    .map_err(|err| err.to_string())?;
                let digest = watch::content_digest(html.as_bytes());
                let ingested = document::open_document(&html, text_viewport(), Limits::default())
                    .map_err(|err| err.to_string())?;
                (
                    ingested,
                    Some((PathBuf::from(input), watch::Baseline { state, digest })),
                )
            } else {
                (ingest_source(input, text_viewport())?, None)
            }
        } else {
            (ingest_source(input, text_viewport())?, None)
        };

    if args.text || !terminal::is_tty(libc::STDOUT_FILENO) {
        return write_plain_text(&ingested.document);
    }

    match TerminalOwner::open_with_config(TerminalConfig::default()) {
        Ok(mut owner) => {
            let result =
                run_pager_session(args, ingested, baseline, &mut owner, started, true, false);
            let finish = owner.finish().map_err(terminal_message);
            match (result, finish) {
                (Err(err), _) => Err(err),
                (Ok(_), Err(err)) => Err(err),
                (Ok(_), Ok(())) => Ok(()),
            }
        }
        Err(err) => {
            eprintln!("glisk: graphics unavailable: {err}; using plain text");
            write_plain_text(&ingested.document)
        }
    }
}

/// Watching is a live-pager feature for named local files only.
fn wants_watch(args: &Args, input: &str) -> bool {
    watch_enabled(args) && input != "-" && !looks_like_url(input)
}

fn watch_enabled(args: &Args) -> bool {
    !args.no_watch && !args.once && !args.scripted_actions
}

fn text_viewport() -> BlitzViewport {
    BlitzViewport::new(
        TEXT_VIEWPORT_WIDTH,
        TEXT_VIEWPORT_HEIGHT,
        1.0,
        ColorScheme::Dark,
    )
}

fn ingest_source(input: &str, viewport: BlitzViewport) -> Result<IngestedDocument, String> {
    if input != "-" && looks_like_url(input) {
        return Err("only local .htm/.html files or `-` for stdin are supported".to_string());
    }
    let result = if input == "-" {
        document::ingest_stdin(viewport, Limits::default())
    } else {
        document::ingest_file(input, viewport, Limits::default())
    };
    result.map_err(|err| IngestError::to_string(&err))
}

/// Reject URL-shaped arguments before they reach the filesystem. Glisk is a
/// local document pager, not a browser.
fn looks_like_url(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    const SCHEMES: [&str; 8] = [
        "http://", "https://", "file://", "data:", "ws://", "wss://", "ftp://", "about:",
    ];
    SCHEMES.iter().any(|scheme| lower.starts_with(scheme))
}

fn write_plain_text(document: &BaseDocument) -> Result<(), String> {
    let text = text::extract_plain_text(document);
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|_| stdout.flush())
    {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(format!("cannot write plain text: {err}")),
    }
}

// ---------------------------------------------------------------------------
// Live pager
// ---------------------------------------------------------------------------

struct Pager {
    view: DocViewport,
    navigation: Navigation,
    search: Search,
    animation_paused: bool,
    frames: u32,
    metrics: Metrics,
    watcher: Option<watch::Watcher>,
    watch_status: Option<String>,
    /// Escape returns to the picker instead of doing nothing.
    back_enabled: bool,
}

impl Pager {
    fn new(document: BaseDocument, width: u32, height: u32, scale: f64) -> Self {
        Self {
            view: DocViewport::new(document, width, height, scale),
            navigation: Navigation::new(),
            search: Search::new(),
            animation_paused: false,
            frames: 0,
            metrics: Metrics::new(),
            watcher: None,
            watch_status: None,
            back_enabled: false,
        }
    }

    fn prepare(&mut self, args: &Args) -> Result<(), String> {
        if let Some(id) = &args.toggle {
            let node = self
                .view
                .document()
                .get_element_by_id(id)
                .ok_or_else(|| format!("no element with id {id:?}"))?;
            self.view.document_mut().toggle_details_open(node);
            self.view.resolve();
        }
        if let Some(selector) = &args.focus {
            let node = self
                .view
                .document()
                .query_selector(selector)
                .map_err(|err| format!("bad selector {selector:?}: {err:?}"))?
                .ok_or_else(|| format!("no element matches {selector:?}"))?;
            self.view.document_mut().set_focus_to(node);
            self.view.resolve();
            self.view.document_mut().scroll_into_view(
                node,
                ScrollBehavior::Instant,
                ScrollLogicalPosition::Nearest,
                ScrollLogicalPosition::Nearest,
            );
        }
        if let Some(fragment) = &args.fragment {
            if !matches!(
                self.navigation.jump_to_fragment(&mut self.view, fragment),
                navigation::FragmentJump::Jumped(_)
            ) {
                return Err(format!("no fragment target {fragment:?}"));
            }
        }
        self.view.resolve();
        Ok(())
    }

    fn draw(&mut self, owner: &mut TerminalOwner, args: &Args) -> Result<(), String> {
        match self
            .view
            .paint()
            .map_err(|err| format!("cannot paint viewport: {err}"))?
        {
            PaintOutcome::Frame(frame) => {
                self.frames += 1;
                if let Some(dir) = args.dump_dir.as_deref() {
                    std::fs::create_dir_all(dir)
                        .map_err(|err| format!("cannot create {dir}: {err}"))?;
                    let path = format!("{dir}/frame-{:03}.png", self.frames);
                    std::fs::write(&path, &frame.png)
                        .map_err(|err| format!("cannot write {path}: {err}"))?;
                }
                if self.frames == 1 {
                    self.metrics
                        .push("png_bytes_first_frame", &frame.png.len().to_string());
                }
                let stats = owner
                    .present(&frame.png)
                    .map_err(|err| format!("cannot write viewport frame: {err}"))?;
                if self.frames == 1 {
                    self.metrics
                        .push("frame_bytes_first_frame", &stats.wire_bytes.to_string());
                }
            }
            PaintOutcome::Skipped(reason) => {
                self.metrics.push("last_paint_skipped", &reason.to_string());
            }
        }
        let status = self.status_text();
        owner
            .paint_status(&status)
            .map_err(|err| format!("cannot write status row: {err}"))
    }

    fn status_text(&mut self) -> String {
        let max_scroll = self.view.max_scroll();
        let scroll = self.view.scroll_y();
        let percent = if max_scroll > 0.0 {
            (scroll / max_scroll * 100.0).round().clamp(0.0, 100.0)
        } else {
            100.0
        };
        let animation_status = if self.animation_paused {
            " | paused (p)"
        } else if self.view.is_animating() {
            " | playing (p)"
        } else {
            ""
        };
        let reload_status = self
            .watch_status
            .as_deref()
            .map(|status| format!(" | {status}"))
            .unwrap_or_default();
        format!(
            "glisk{reload_status} | {percent:.0}%{animation_status} | {} | {} | Tab focus  Enter open  / search  j/k scroll  q quit",
            self.search.status(),
            self.navigation.status()
        )
    }

    fn run(
        &mut self,
        args: &Args,
        owner: &mut TerminalOwner,
        started: Instant,
        write_metrics: bool,
    ) -> Result<SessionExit, String> {
        self.metrics.push(
            "term_program",
            &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        );
        self.metrics
            .push("term", &std::env::var("TERM").unwrap_or_default());

        self.animation_paused = args.pause_animation;
        self.draw(owner, args)?;
        self.metrics.push(
            "cold_start_to_frame_write_ms",
            &format_ms(started.elapsed()),
        );

        if args.scripted_actions {
            self.run_scripted_actions(args, owner)?;
            self.metrics.push("frames", &self.frames.to_string());
            note_rss(&mut self.metrics);
            if write_metrics {
                self.metrics.write(args.metrics.as_deref())?;
            }
            return Ok(SessionExit::Quit);
        }

        if args.once {
            let _ = io::stdout().flush();
            std::thread::sleep(Duration::from_millis(700));
            self.metrics.push("frames", &self.frames.to_string());
            note_rss(&mut self.metrics);
            if write_metrics {
                self.metrics.write(args.metrics.as_deref())?;
            }
            return Ok(SessionExit::Quit);
        }

        let mut last_sample = Instant::now();
        let mut last_frame = last_sample;
        let exit = loop {
            let timeout = if self.animation_active(owner) {
                ANIMATION_FRAME_INTERVAL.saturating_sub(last_frame.elapsed())
            } else {
                Duration::from_millis(100)
            };
            let batch = owner.poll_input(timeout).map_err(terminal_message)?;
            let mut changed = false;
            let mut quit = false;
            let mut back = false;
            let now = Instant::now();
            if !self.animation_paused {
                self.view.advance_animation(now.duration_since(last_sample));
            }
            last_sample = now;

            // Resolve a pending terminal resize before mapping mouse reports,
            // so a click always uses the grid that produced the visible frame.
            if let Some(geometry) = owner.refresh_size().map_err(terminal_message)? {
                let (width, height) = geometry.physical_size();
                if matches!(
                    self.view.resize(width, height, geometry.scale()),
                    ResizeOutcome::Applied(_)
                ) {
                    changed = true;
                }
            }

            if let Some(batch) = batch {
                if batch.interrupt {
                    quit = true;
                } else {
                    for input in parse_input(&batch.bytes) {
                        changed |= self.handle_input(input, owner.geometry(), &mut quit, &mut back);
                    }
                }
            }
            if quit {
                break SessionExit::Quit;
            }
            if back {
                break SessionExit::Back;
            }
            if owner.take_repaint_request() {
                self.metrics.count("tmux_repaints");
                changed = true;
            }

            if let Some(state) = self.watcher.as_mut().and_then(|watcher| watcher.poll(now)) {
                let path = self
                    .watcher
                    .as_ref()
                    .map(|watcher| watcher.path().to_path_buf())
                    .unwrap_or_default();
                match self.reload(&path, &state) {
                    Ok(reloaded) => {
                        if self.watch_status.take().is_some() {
                            changed = true;
                        }
                        changed |= reloaded;
                    }
                    Err(status) => {
                        if let Some(watcher) = &mut self.watcher {
                            watcher.reject(state);
                        }
                        if self.watch_status.as_deref() != Some(status.as_str()) {
                            self.watch_status = Some(status);
                            changed = true;
                        }
                    }
                }
            }

            if changed
                || (self.animation_active(owner)
                    && last_frame.elapsed() >= ANIMATION_FRAME_INTERVAL)
            {
                self.draw(owner, args)?;
                last_frame = Instant::now();
            }
        };

        self.metrics.push("frames", &self.frames.to_string());
        note_rss(&mut self.metrics);
        if write_metrics {
            self.metrics.write(args.metrics.as_deref())?;
        }
        Ok(exit)
    }

    fn animation_active(&self, owner: &TerminalOwner) -> bool {
        !self.animation_paused
            && self.view.geometry().is_some()
            && owner.tmux_visible() != Some(false)
            && self.view.is_animating()
    }

    fn handle_input(
        &mut self,
        input: Input,
        geometry: Geometry,
        quit: &mut bool,
        back: &mut bool,
    ) -> bool {
        if self.search.is_editing() {
            return match input {
                Input::Enter => {
                    let text = text::extract(self.view.document());
                    self.search
                        .commit(&text, &mut self.view, &mut self.navigation);
                    true
                }
                Input::Escape => {
                    self.search.cancel();
                    true
                }
                Input::Backspace => self.search.backspace(),
                Input::Char(ch) => self.search.push_char(ch),
                _ => false,
            };
        }

        match input {
            Input::Char('q') => {
                *quit = true;
                false
            }
            Input::Escape if self.back_enabled => {
                *back = true;
                false
            }
            Input::Escape => false,
            Input::Char('j') | Input::Down => self.view.scroll_by(SCROLL_STEP),
            Input::Char('k') | Input::Up => self.view.scroll_by(-SCROLL_STEP),
            Input::Char(' ') | Input::PageDown => self.view.scroll_page(1.0),
            Input::PageUp => self.view.scroll_page(-1.0),
            Input::Char('g') | Input::Home => self.view.scroll_home(),
            Input::Char('G') | Input::End => self.view.scroll_end(),
            Input::Char('p') => {
                self.animation_paused = !self.animation_paused;
                true
            }
            Input::Tab => {
                self.navigation.focus_next(&mut self.view);
                true
            }
            Input::BackTab => {
                self.navigation.focus_prev(&mut self.view);
                true
            }
            Input::Enter => {
                self.navigation.activate(&mut self.view);
                true
            }
            Input::Char('/') => {
                self.search.begin();
                true
            }
            Input::Char('n') => {
                self.search.next(&mut self.view, &mut self.navigation);
                true
            }
            Input::Char('N') => {
                self.search.previous(&mut self.view, &mut self.navigation);
                true
            }
            Input::Mouse { action, col, row } => {
                let Some((x, y)) = geometry.content_cell_center(col, row) else {
                    return false;
                };
                match action {
                    MouseAction::LeftPress => self.navigation.click(&mut self.view, x, y),
                    MouseAction::WheelUp => self.view.scroll_by(-SCROLL_STEP),
                    MouseAction::WheelDown => self.view.scroll_by(SCROLL_STEP),
                }
            }
            _ => false,
        }
    }

    /// Open one changed revision through the bounded offline path, map reader
    /// state onto it, and swap only after it can paint. The previous report is
    /// untouched on every failure.
    fn reload(&mut self, path: &Path, state: &watch::PathState) -> Result<bool, String> {
        if !matches!(state, watch::PathState::File(_)) {
            return Err(match state {
                watch::PathState::Missing => {
                    "reload: file missing (keeping previous report)".to_string()
                }
                watch::PathState::NotRegular => {
                    "reload: not a regular file (keeping previous report)".to_string()
                }
                watch::PathState::Unreadable => {
                    "reload: cannot read file (keeping previous report)".to_string()
                }
                watch::PathState::File(_) => unreachable!(),
            });
        }
        let Some(geometry) = self.view.geometry() else {
            return Err("reload: no viewport (keeping previous report)".to_string());
        };
        let html = document::read_html_file(path, &Limits::default())
            .map_err(|err| reload_ingest_error(&err))?;
        let digest = watch::content_digest(html.as_bytes());
        if self
            .watcher
            .as_ref()
            .is_some_and(|watcher| watcher.known_digest() == digest)
        {
            if let Some(watcher) = &mut self.watcher {
                watcher.accept(state.clone(), digest);
            }
            self.metrics.count("watch_unchanged");
            return Ok(false);
        }

        let reading = capture_reload_state(&mut self.view, &self.search);
        let ingested =
            document::open_document(&html, geometry_viewport(geometry), Limits::default())
                .map_err(|err| reload_ingest_error(&err))?;
        let (width, height) = geometry.physical_size();
        let mut candidate = DocViewport::new(ingested.document, width, height, geometry.scale());
        restore_open_details(&mut candidate, &reading.open_details);
        restore_focus(&mut candidate, reading.focus.as_ref());
        if let Some(anchor) = &reading.anchor {
            candidate.restore_author_anchor(anchor);
        }
        let text = text::extract(candidate.document());
        let mut staged_search = self.search.clone();
        staged_search.remap(&text, &mut candidate, reading.match_author);
        match candidate
            .paint()
            .map_err(|err| format!("reload: cannot render ({err}); keeping previous report"))?
        {
            PaintOutcome::Frame(_) => {}
            PaintOutcome::Skipped(reason) => {
                return Err(format!(
                    "reload: cannot render ({reason}); keeping previous report"
                ));
            }
        }

        self.view = candidate;
        self.search = staged_search;
        if let Some(watcher) = &mut self.watcher {
            watcher.accept(state.clone(), digest);
        }
        self.metrics.count("watch_reloads");
        Ok(true)
    }

    fn run_scripted_actions(
        &mut self,
        args: &Args,
        owner: &mut TerminalOwner,
    ) -> Result<(), String> {
        let mut durations = Vec::new();
        for action in ScriptedAction::SEQUENCE {
            let at = Instant::now();
            self.apply_scripted(action);
            self.draw(owner, args)?;
            durations.push(at.elapsed());
        }
        let average = durations.iter().map(Duration::as_secs_f64).sum::<f64>()
            / durations.len().max(1) as f64
            * 1000.0;
        self.metrics
            .push("scripted_action_frames", &durations.len().to_string());
        self.metrics.push(
            "scripted_action_to_frame_write_avg_ms",
            &format!("{average:.1}"),
        );
        Ok(())
    }

    fn apply_scripted(&mut self, action: ScriptedAction) {
        match action {
            ScriptedAction::FocusNext => {
                self.navigation.focus_next(&mut self.view);
            }
            ScriptedAction::Activate => {
                self.navigation.activate(&mut self.view);
            }
            ScriptedAction::Anchor(fragment) => {
                self.navigation.jump_to_fragment(&mut self.view, fragment);
            }
            ScriptedAction::Scroll(delta) => {
                self.view.scroll_by(delta);
            }
            ScriptedAction::Toggle(id) => {
                if let Some(node) = self.view.document().get_element_by_id(id) {
                    self.view.document_mut().toggle_details_open(node);
                    self.view.resolve();
                }
            }
        }
    }
}

/// Reader state captured from the displayed revision for a reload.
struct ReloadState {
    anchor: Option<AuthorAnchor>,
    open_details: Vec<String>,
    focus: Option<FocusPoint>,
    match_author: Option<String>,
}

struct FocusPoint {
    author: Option<String>,
    index: Option<usize>,
    kind: Option<navigation::ControlKind>,
}

fn capture_reload_state(view: &mut DocViewport, search: &Search) -> ReloadState {
    view.resolve();
    let anchor = view.capture_author_anchor();
    let open_details = open_details_ids(view.document());
    let controls = navigation::controls(view.document());
    let focus = view
        .document()
        .get_focussed_node_id()
        .map(|node| FocusPoint {
            author: viewport::author_id(view.document(), node),
            index: controls.iter().position(|control| control.node == node),
            kind: controls
                .iter()
                .find(|control| control.node == node)
                .map(|control| control.kind),
        });
    let match_author = search
        .current_match()
        .and_then(|node| viewport::author_id(view.document(), node));
    ReloadState {
        anchor,
        open_details,
        focus,
        match_author,
    }
}

fn open_details_ids(document: &BaseDocument) -> Vec<String> {
    document
        .tree()
        .iter()
        .filter_map(|(_, node)| {
            let element = node.element_data()?;
            let local: &str = element.name.local.as_ref();
            if local != "details" || !node.data.has_attr(local_name!("open")) {
                return None;
            }
            viewport::own_id(node).map(str::to_owned)
        })
        .collect()
}

fn restore_open_details(view: &mut DocViewport, ids: &[String]) {
    let mut opened = false;
    for id in ids {
        let Some(node) = view.document().get_element_by_id(id) else {
            continue;
        };
        let is_details = view
            .document()
            .get_node(node)
            .and_then(|node| node.element_data())
            .is_some_and(|element| {
                let local: &str = element.name.local.as_ref();
                local == "details"
            });
        if !is_details {
            continue;
        }
        let already_open = view
            .document()
            .get_node(node)
            .is_some_and(|node| node.data.has_attr(local_name!("open")));
        if !already_open {
            view.document_mut().toggle_details_open(node);
            opened = true;
        }
    }
    if opened {
        view.resolve();
    }
}

fn restore_focus(view: &mut DocViewport, focus: Option<&FocusPoint>) {
    let Some(focus) = focus else {
        return;
    };
    view.resolve();
    let controls = navigation::controls(view.document());
    let by_id = focus.author.as_deref().and_then(|author| {
        let node = view.document().get_element_by_id(author)?;
        controls
            .iter()
            .any(|control| control.node == node)
            .then_some(node)
    });
    let by_index = focus.index.and_then(|index| {
        controls
            .get(index)
            .filter(|control| Some(control.kind) == focus.kind)
            .map(|control| control.node)
    });
    if let Some(node) = by_id.or(by_index) {
        view.document_mut().set_focus_to(node);
    }
}

fn geometry_viewport(geometry: ViewGeometry) -> BlitzViewport {
    let (width, height) = geometry.physical_size();
    BlitzViewport::new(width, height, geometry.scale() as f32, ColorScheme::Dark)
}

fn reload_ingest_error(err: &IngestError) -> String {
    let reason = match err {
        IngestError::Read(io) if io.kind() == io::ErrorKind::NotFound => "file missing",
        IngestError::Read(_) => "cannot read file",
        IngestError::TooLarge { .. } => "file too large",
        IngestError::InvalidUtf8 { .. } => "file is not UTF-8",
        IngestError::NodeBudget { .. } | IngestError::DepthBudget { .. } => "document too complex",
    };
    format!("reload: {reason} (keeping previous report)")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionExit {
    /// `q`, Ctrl+C, or a hidden one-shot diagnostic ends Glisk.
    Quit,
    /// Escape in a report opened from the picker returns to the list.
    Back,
}

/// Run one live document session on an owner that stays open. The caller owns
/// finish/cleanup, so the picker can reuse the same terminal owner between
/// reports.
fn run_pager_session(
    args: &Args,
    ingested: IngestedDocument,
    baseline: Option<(PathBuf, watch::Baseline)>,
    owner: &mut TerminalOwner,
    started: Instant,
    write_metrics: bool,
    back_enabled: bool,
) -> Result<SessionExit, String> {
    let geometry = owner.geometry();
    let (width, height) = geometry.physical_size();
    let mut pager = Pager::new(ingested.document, width, height, geometry.scale());
    pager.back_enabled = back_enabled;
    match baseline {
        Some((path, baseline)) if watch_enabled(args) => {
            pager.watcher = Some(watch::Watcher::new(path, baseline));
            pager.metrics.push("watch", "on");
        }
        _ => pager.metrics.push("watch", "off"),
    }
    pager.metrics.push("mode", "live");
    pager.metrics.push("placement", owner.placement_name());
    pager
        .metrics
        .push("image_id", &owner.image_id().to_string());
    if let Some(summary) = owner.tmux_summary() {
        pager.metrics.push("tmux", &summary);
        pager.metrics.push(
            "tmux_visible_initial",
            &owner.tmux_visible().unwrap_or(false).to_string(),
        );
    }
    pager.metrics.push("cols", &geometry.cols().to_string());
    pager.metrics.push("rows", &geometry.rows().to_string());
    let cell = geometry.cell();
    pager
        .metrics
        .push("cell_width_px", &cell.width().to_string());
    pager
        .metrics
        .push("cell_height_px", &cell.height().to_string());
    let (css_width, css_height) = geometry.css_size();
    pager.metrics.push("css_width", &format!("{css_width:.1}"));
    pager
        .metrics
        .push("css_height", &format!("{css_height:.1}"));
    pager
        .metrics
        .push("scale", &format!("{:.3}", geometry.scale()));
    pager
        .metrics
        .push("document_nodes", &ingested.stats.nodes.to_string());

    pager.prepare(args)?;
    pager.run(args, owner, started, write_metrics)
}

// ---------------------------------------------------------------------------
// Report picker
// ---------------------------------------------------------------------------

fn run_picker(args: &Args, root: PathBuf, started: Instant) -> Result<(), String> {
    let mut owner = TerminalOwner::open_with_config(TerminalConfig::default()).map_err(|err| {
        format!(
            "picker needs a supported interactive terminal ({err}); {}",
            picker_needs_report_hint()
        )
    })?;
    let mut picker = picker::Picker::new(root);
    picker.refresh();
    let mut metrics = Metrics::new();
    metrics.push("mode", "picker");
    metrics.push("picker_root", &picker.root().display().to_string());
    metrics.push("picker_reports", &picker.entry_count().to_string());
    metrics.push("picker_partial", &picker.partial().to_string());
    metrics.push("placement", owner.placement_name());

    let result = picker_loop(args, &mut picker, &mut owner, started, &mut metrics);
    note_rss(&mut metrics);
    if result.is_ok() {
        metrics.write(args.metrics.as_deref())?;
    }
    let finish = owner.finish().map_err(terminal_message);
    match (result, finish) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(err),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn picker_loop(
    args: &Args,
    picker: &mut picker::Picker,
    owner: &mut TerminalOwner,
    started: Instant,
    metrics: &mut Metrics,
) -> Result<(), String> {
    loop {
        draw_picker(picker, owner)?;
        let mut open = false;
        loop {
            let batch = owner
                .poll_input(Duration::from_millis(100))
                .map_err(terminal_message)?;
            let mut changed = false;
            if owner.refresh_size().map_err(terminal_message)?.is_some() {
                changed = true;
            }
            if let Some(batch) = batch {
                if batch.interrupt {
                    return Ok(());
                }
                for input in parse_input(&batch.bytes) {
                    let Some(key) = picker_key(input) else {
                        continue;
                    };
                    let page = usize::from(owner.geometry().rows().saturating_sub(4)).max(1);
                    match picker.handle(key, page) {
                        picker::Action::Exit => return Ok(()),
                        picker::Action::Open => {
                            open = true;
                            break;
                        }
                        picker::Action::Changed => changed = true,
                        picker::Action::None => {}
                    }
                }
            }
            if open {
                break;
            }
            if owner.take_repaint_request() {
                metrics.count("tmux_repaints");
                changed = true;
            }
            if changed {
                draw_picker(picker, owner)?;
            }
        }

        let Some(path) = picker.selected_path().map(Path::to_path_buf) else {
            continue;
        };
        metrics.count("picker_opens");
        match open_picker_report(args, &path, owner, started) {
            Ok(SessionExit::Quit) => return Ok(()),
            Ok(SessionExit::Back) => {
                metrics.count("picker_returns");
                owner.clear_image().map_err(terminal_message)?;
                picker.refresh();
            }
            Err(err) => {
                owner.clear_image().map_err(terminal_message)?;
                picker.set_message(short_picker_error(&err));
                picker.refresh();
            }
        }
    }
}

fn draw_picker(picker: &mut picker::Picker, owner: &mut TerminalOwner) -> Result<(), String> {
    let geometry = owner.geometry();
    let lines = picker.render(geometry.cols(), geometry.rows());
    owner.paint_screen(&lines).map_err(terminal_message)
}

/// Recheck the selected path, then ingest it through the same bounded offline
/// path as a named file. The picker never keeps a report open on failure.
fn open_picker_report(
    args: &Args,
    path: &Path,
    owner: &mut TerminalOwner,
    started: Instant,
) -> Result<SessionExit, String> {
    let state = watch::path_state(path);
    if !matches!(state, watch::PathState::File(_)) {
        return Err(match state {
            watch::PathState::Missing => "report removed before opening".to_string(),
            watch::PathState::NotRegular => "report is not a regular file".to_string(),
            watch::PathState::Unreadable => "report is unreadable".to_string(),
            watch::PathState::File(_) => unreachable!(),
        });
    }
    let limits = Limits::default();
    let html = document::read_html_file(path, &limits)
        .map_err(|err| picker_ingest_error(&err, "cannot open report"))?;
    let digest = watch::content_digest(html.as_bytes());
    let ingested = document::open_document(&html, text_viewport(), limits)
        .map_err(|err| picker_ingest_error(&err, "cannot open report"))?;
    let baseline = if watch_enabled(args) {
        Some((path.to_path_buf(), watch::Baseline { state, digest }))
    } else {
        None
    };
    run_pager_session(args, ingested, baseline, owner, started, false, true)
}

fn picker_ingest_error(err: &IngestError, prefix: &str) -> String {
    let reason = match err {
        IngestError::Read(io) if io.kind() == io::ErrorKind::NotFound => "file missing",
        IngestError::Read(_) => "cannot read file",
        IngestError::TooLarge { .. } => "file too large",
        IngestError::InvalidUtf8 { .. } => "file is not UTF-8",
        IngestError::NodeBudget { .. } | IngestError::DepthBudget { .. } => "document too complex",
    };
    format!("{prefix}: {reason}")
}

/// Keep picker errors short enough for one status row.
fn short_picker_error(message: &str) -> String {
    picker::sanitize_text(message)
}

fn picker_key(input: Input) -> Option<picker::Key> {
    Some(match input {
        Input::Up => picker::Key::Up,
        Input::Down => picker::Key::Down,
        Input::PageUp => picker::Key::PageUp,
        Input::PageDown => picker::Key::PageDown,
        Input::Home => picker::Key::Home,
        Input::End => picker::Key::End,
        Input::Enter => picker::Key::Enter,
        Input::Escape => picker::Key::Escape,
        Input::Backspace => picker::Key::Backspace,
        Input::Char(ch) => picker::Key::Char(ch),
        _ => picker::Key::Other,
    })
}

fn terminal_message(err: TerminalError) -> String {
    format!("terminal: {err}")
}

#[derive(Clone, Copy)]
enum ScriptedAction {
    FocusNext,
    Activate,
    Anchor(&'static str),
    Scroll(f64),
    Toggle(&'static str),
}

impl ScriptedAction {
    const SEQUENCE: [Self; 9] = [
        Self::FocusNext,
        Self::Activate,
        Self::FocusNext,
        Self::Activate,
        Self::Anchor("chart"),
        Self::Scroll(40.0),
        Self::Scroll(-40.0),
        Self::Anchor("evidence"),
        Self::Toggle("evidence-method"),
    ];
}

// ---------------------------------------------------------------------------
// Terminal input
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseAction {
    LeftPress,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    Escape,
    Enter,
    Tab,
    BackTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Char(char),
    Mouse {
        action: MouseAction,
        col: u16,
        row: u16,
    },
    Unknown,
}

/// Parse the keyboard and mouse bytes returned by the terminal owner. It has
/// already separated protocol replies and retained incomplete sequences, so
/// every escape sequence here is complete.
fn parse_input(bytes: &[u8]) -> Vec<Input> {
    let mut events = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                if i + 1 >= bytes.len() {
                    events.push(Input::Escape);
                    i += 1;
                    continue;
                }
                match bytes[i + 1] {
                    b'[' => {
                        let start = i + 2;
                        let mut end = start;
                        while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                            end += 1;
                        }
                        if end >= bytes.len() {
                            events.push(Input::Unknown);
                            break;
                        }
                        let params = &bytes[start..end];
                        events.push(match (params.strip_prefix(b"<"), bytes[end]) {
                            (Some(report), b'M') => parse_mouse(report).unwrap_or(Input::Unknown),
                            (Some(_), _) => Input::Unknown,
                            (None, b'A') => Input::Up,
                            (None, b'B') => Input::Down,
                            (None, b'C') => Input::Right,
                            (None, b'D') => Input::Left,
                            (None, b'H') => Input::Home,
                            (None, b'F') => Input::End,
                            (None, b'Z') => Input::BackTab,
                            (None, b'~') => match params {
                                b"1" | b"7" => Input::Home,
                                b"4" | b"8" => Input::End,
                                b"5" => Input::PageUp,
                                b"6" => Input::PageDown,
                                b"3" => Input::Backspace,
                                _ => Input::Unknown,
                            },
                            _ => Input::Unknown,
                        });
                        i = end + 1;
                    }
                    b'O' => {
                        if i + 2 >= bytes.len() {
                            events.push(Input::Escape);
                            i += 1;
                            continue;
                        }
                        events.push(match bytes[i + 2] {
                            b'A' => Input::Up,
                            b'B' => Input::Down,
                            b'C' => Input::Right,
                            b'D' => Input::Left,
                            b'H' => Input::Home,
                            b'F' => Input::End,
                            _ => Input::Unknown,
                        });
                        i += 3;
                    }
                    _ => {
                        // Alt+key: report Escape, then the key on the next pass.
                        events.push(Input::Escape);
                        i += 1;
                    }
                }
            }
            b'\t' => {
                events.push(Input::Tab);
                i += 1;
            }
            b'\r' | b'\n' => {
                events.push(Input::Enter);
                i += 1;
            }
            0x7f | 0x08 => {
                events.push(Input::Backspace);
                i += 1;
            }
            _ => {
                let start = i;
                while i < bytes.len()
                    && !matches!(bytes[i], 0x1b | b'\t' | b'\r' | b'\n' | 0x7f | 0x08)
                {
                    i += 1;
                }
                for ch in String::from_utf8_lossy(&bytes[start..i]).chars() {
                    events.push(Input::Char(ch));
                }
            }
        }
    }
    events
}

fn parse_mouse(report: &[u8]) -> Option<Input> {
    let mut fields = std::str::from_utf8(report).ok()?.split(';');
    let action = match fields.next()? {
        "0" => MouseAction::LeftPress,
        "64" => MouseAction::WheelUp,
        "65" => MouseAction::WheelDown,
        _ => return None,
    };
    let col = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    if fields.next().is_some() {
        return None;
    }
    Some(Input::Mouse { action, col, row })
}

// ---------------------------------------------------------------------------
// Legacy headless diagnostics
// ---------------------------------------------------------------------------

fn self_test(args: &Args, started: Instant) -> Result<(), String> {
    use blitz_dom::{ScrollBehavior, ScrollLogicalPosition, local_name};

    let mut metrics = Metrics::new();
    metrics.push("mode", "self-test");

    let scale = 2.0;
    let (narrow_w, narrow_h) = (840, 1792);
    let (wide_w, wide_h) = (2000, 1792);
    let ingested = ingest_source(
        required_input(args)?,
        BlitzViewport::new(narrow_w, narrow_h, scale as f32, ColorScheme::Dark),
    )?;
    let mut doc = ingested.document;

    let link = doc
        .query_selector("a[href='#comparison']")
        .map_err(|e| format!("selector error: {e:?}"))?
        .ok_or("anchor link missing")?;
    doc.set_focus_to(link);
    activate(&mut doc);
    doc.resolve(0.0);
    let anchored = doc.viewport_scroll().y;
    if anchored <= 0.0 {
        return Err("anchor activation did not scroll".to_string());
    }
    metrics.push("anchor_scroll_y", &format!("{anchored:.1}"));

    let details = doc
        .get_element_by_id("evidence-method")
        .ok_or("disclosure missing")?;
    let summary = doc
        .query_selector("#evidence-method summary")
        .map_err(|e| format!("selector error: {e:?}"))?
        .ok_or("disclosure summary missing")?;
    doc.set_focus_to(summary);
    doc.toggle_details_open(details);
    doc.resolve(0.0);
    let is_open = |doc: &BaseDocument| {
        doc.get_node(details)
            .map(|n| n.data.has_attr(local_name!("open")))
            .unwrap_or(false)
    };
    if !is_open(&doc) {
        return Err("details did not open".to_string());
    }
    let focus_before = doc.get_focussed_node_id();

    doc.set_viewport(BlitzViewport::new(
        wide_w,
        wide_h,
        scale as f32,
        ColorScheme::Dark,
    ));
    doc.resolve(0.0);
    doc.scroll_into_view(
        details,
        ScrollBehavior::Instant,
        ScrollLogicalPosition::Nearest,
        ScrollLogicalPosition::Nearest,
    );
    doc.resolve(0.0);

    if !is_open(&doc) {
        return Err("details closed across resize".to_string());
    }
    if doc.get_focussed_node_id() != focus_before {
        return Err("focus lost across resize".to_string());
    }
    metrics.push("open_disclosure_after_resize", "true");
    metrics.push("focus_retained_after_resize", "true");
    metrics.push("resize_css_width", &format!("{:.1}", wide_w as f64 / scale));
    metrics.push(
        "resize_css_height",
        &format!("{:.1}", wide_h as f64 / scale),
    );
    metrics.push("cold_start_to_self_test_ms", &format_ms(started.elapsed()));
    metrics.push("result", "pass");
    note_rss(&mut metrics);
    metrics.write(args.metrics.as_deref())?;
    eprintln!("glisk: self-test passed");
    Ok(())
}

fn snapshot(args: &Args, path: &str, started: Instant) -> Result<(), String> {
    let width = (args.css_width * args.scale).round() as u32;
    let height = (args.css_height * args.scale).round() as u32;
    check_viewport(width, height)?;
    let ingested = ingest_source(
        required_input(args)?,
        BlitzViewport::new(width, height, args.scale as f32, ColorScheme::Dark),
    )?;
    let mut doc = ingested.document;
    prepare(&mut doc, args)?;
    let png = render(&mut doc, width, height, args.scale)?;
    std::fs::write(path, &png).map_err(|e| format!("cannot write {path}: {e}"))?;
    let elapsed = started.elapsed();
    let mut metrics = Metrics::new();
    metrics.push("mode", "snapshot");
    metrics.push("css_width", &args.css_width.to_string());
    metrics.push("css_height", &args.css_height.to_string());
    metrics.push("scale", &args.scale.to_string());
    metrics.push("physical_width", &width.to_string());
    metrics.push("physical_height", &height.to_string());
    metrics.push("png_bytes", &png.len().to_string());
    metrics.push("cold_start_to_png_ms", &format_ms(elapsed));
    note_rss(&mut metrics);
    metrics.write(args.metrics.as_deref())?;
    eprintln!(
        "glisk: snapshot {width}x{height}px {} bytes in {:.1} ms -> {path}",
        png.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}

/// Full-page sweep: paint one viewport-sized frame per page from the top to the
/// bottom of the document. Every frame reuses the same geometry, so the largest
/// raster and encoded frame show whether memory tracks the viewport or document
/// length. Release evidence in `docs/release.md` uses this for long reports.
fn scroll_sweep(args: &Args, started: Instant) -> Result<(), String> {
    let width = (args.css_width * args.scale).round() as u32;
    let height = (args.css_height * args.scale).round() as u32;
    check_viewport(width, height)?;
    let ingested = ingest_source(
        required_input(args)?,
        BlitzViewport::new(width, height, args.scale as f32, ColorScheme::Dark),
    )?;
    let mut metrics = Metrics::new();
    metrics.push("mode", "scroll-sweep");
    metrics.push("css_width", &args.css_width.to_string());
    metrics.push("css_height", &args.css_height.to_string());
    metrics.push("scale", &args.scale.to_string());
    metrics.push("physical_width", &width.to_string());
    metrics.push("physical_height", &height.to_string());
    metrics.push("document_nodes", &ingested.stats.nodes.to_string());

    let mut view = DocViewport::new(ingested.document, width, height, args.scale);
    let mut frames = 0u32;
    let mut max_png = 0usize;
    let mut max_raster = 0u64;
    let mut last_scroll = 0.0;
    let mut skipped = 0u32;
    loop {
        match view
            .paint()
            .map_err(|err| format!("cannot paint viewport: {err}"))?
        {
            PaintOutcome::Frame(frame) => {
                frames += 1;
                max_png = max_png.max(frame.png.len());
                max_raster = max_raster.max(frame.pixels);
                last_scroll = frame.scroll_y;
            }
            PaintOutcome::Skipped(_) => skipped += 1,
        }
        if !view.scroll_page(1.0) {
            break;
        }
    }

    metrics.push("frames", &frames.to_string());
    metrics.push("skipped_frames", &skipped.to_string());
    metrics.push("max_png_bytes", &max_png.to_string());
    metrics.push("max_raster_pixels", &max_raster.to_string());
    metrics.push("final_scroll_y", &format!("{last_scroll:.1}"));
    metrics.push("max_scroll_y", &format!("{:.1}", view.max_scroll()));
    metrics.push("cold_start_to_sweep_end_ms", &format_ms(started.elapsed()));
    note_rss(&mut metrics);
    metrics.write(args.metrics.as_deref())?;
    eprintln!(
        "glisk: scroll sweep {frames} frames, max raster {max_raster}px, max PNG {max_png} B"
    );
    Ok(())
}

fn check_viewport(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(format!("invalid viewport {w}x{h}"));
    }
    if u64::from(w) * u64::from(h) > MAX_RASTER_PIXELS {
        return Err(format!("viewport {w}x{h} exceeds the 8,388,608 pixel cap"));
    }
    Ok(())
}

fn prepare(doc: &mut BaseDocument, args: &Args) -> Result<(), String> {
    if let Some(id) = &args.toggle {
        let node = doc
            .get_element_by_id(id)
            .ok_or_else(|| format!("no element with id {id:?}"))?;
        doc.toggle_details_open(node);
    }
    if let Some(selector) = &args.focus {
        let node = doc
            .query_selector(selector)
            .map_err(|e| format!("bad selector {selector:?}: {e:?}"))?
            .ok_or_else(|| format!("no element matches {selector:?}"))?;
        doc.set_focus_to(node);
        doc.resolve(0.0);
        doc.scroll_into_view(
            node,
            ScrollBehavior::Instant,
            ScrollLogicalPosition::Nearest,
            ScrollLogicalPosition::Nearest,
        );
    }
    if let Some(fragment) = &args.fragment {
        doc.resolve(0.0);
        if !doc.scroll_to_fragment(fragment) {
            return Err(format!("no fragment target {fragment:?}"));
        }
    }
    doc.resolve(0.0);
    Ok(())
}

fn render(doc: &mut BaseDocument, phys_w: u32, phys_h: u32, scale: f64) -> Result<Vec<u8>, String> {
    let mut painter = VelloCpuScenePainter::new(phys_w as u16, phys_h as u16);
    blitz_paint::paint_scene(&mut painter, doc, scale, phys_w, phys_h, 0, 0);
    let pixmap = painter.finish();
    pixmap
        .into_png()
        .map_err(|e| format!("PNG encode failed: {e:?}"))
}

fn tag_name(doc: &BaseDocument, id: blitz_dom::NodeId) -> Option<String> {
    doc.get_node(id)
        .and_then(|n| n.element_data())
        .map(|el| el.name.local.to_string())
}

fn href(doc: &BaseDocument, id: blitz_dom::NodeId) -> Option<String> {
    doc.get_node(id)
        .and_then(|n| n.element_data())
        .and_then(|el| {
            el.attrs.iter().find(|a| {
                let local: &str = a.name.local.as_ref();
                local == "href"
            })
        })
        .map(|a| a.value.clone())
}

fn activate(doc: &mut BaseDocument) {
    let Some(id) = doc.get_focussed_node_id() else {
        return;
    };
    match tag_name(doc, id).as_deref() {
        Some("details") => doc.toggle_details_open(id),
        Some("summary") => {
            if let Some(details) = doc
                .node_chain(id)
                .into_iter()
                .find(|n| tag_name(doc, *n).as_deref() == Some("details"))
            {
                doc.toggle_details_open(details);
            }
        }
        Some("a") => {
            if let Some(fragment) = href(doc, id).and_then(|target| {
                target
                    .strip_prefix('#')
                    .map(std::string::ToString::to_string)
            }) {
                doc.resolve(0.0);
                doc.scroll_to_fragment(&fragment);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Metrics {
    lines: Vec<(String, String)>,
}

impl Metrics {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, key: &str, value: &str) {
        self.lines.push((key.to_string(), value.to_string()));
    }

    /// Increment a counter line; written as `key=<n>` at exit.
    fn count(&mut self, key: &str) {
        if let Some((_, value)) = self.lines.iter_mut().find(|(name, _)| name == key) {
            let next = value.parse::<u64>().unwrap_or(0) + 1;
            *value = next.to_string();
        } else {
            self.push(key, "1");
        }
    }

    fn write(&self, path: Option<&str>) -> Result<(), String> {
        let Some(path) = path else { return Ok(()) };
        let body: String = self
            .lines
            .iter()
            .map(|(key, value)| format!("{key}={value}\n"))
            .collect();
        std::fs::write(path, body).map_err(|err| format!("cannot write {path}: {err}"))
    }
}

fn format_ms(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64() * 1000.0)
}

fn note_rss(metrics: &mut Metrics) {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc == 0 {
        #[cfg(target_os = "macos")]
        let bytes = usage.ru_maxrss as u64;
        #[cfg(not(target_os = "macos"))]
        let bytes = usage.ru_maxrss as u64 * 1024;
        metrics.push("peak_rss_bytes", &bytes.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_navigation_and_search_keys() {
        let keys = parse_input(b"\x1b[A\x1b[B\x1b[5~\x1b[6~\x1b[Z\t\r/abc\x7f\x1b");
        assert_eq!(
            keys,
            vec![
                Input::Up,
                Input::Down,
                Input::PageUp,
                Input::PageDown,
                Input::BackTab,
                Input::Tab,
                Input::Enter,
                Input::Char('/'),
                Input::Char('a'),
                Input::Char('b'),
                Input::Char('c'),
                Input::Backspace,
                Input::Escape,
            ]
        );
    }

    #[test]
    fn parses_mouse_presses_and_wheel_reports() {
        let inputs = parse_input(
            b"\x1b[<0;12;5M\x1b[<0;12;5m\x1b[<64;1;1M\x1b[<65;80;24M\x1b[<32;2;2M\x1b[<0;0;1M",
        );
        assert_eq!(
            inputs,
            vec![
                Input::Mouse {
                    action: MouseAction::LeftPress,
                    col: 11,
                    row: 4,
                },
                Input::Unknown,
                Input::Mouse {
                    action: MouseAction::WheelUp,
                    col: 0,
                    row: 0,
                },
                Input::Mouse {
                    action: MouseAction::WheelDown,
                    col: 79,
                    row: 23,
                },
                Input::Unknown,
                Input::Unknown,
            ]
        );
    }

    #[test]
    fn rejects_url_shaped_inputs() {
        assert!(looks_like_url("https://example.invalid/report.html"));
        assert!(looks_like_url("file:///tmp/report.html"));
        assert!(looks_like_url("data:text/html,<p>x</p>"));
        assert!(!looks_like_url("report.html"));
        assert!(!looks_like_url("-"));
    }

    #[test]
    fn viewport_budget_is_enforced() {
        assert!(check_viewport(840, 1800).is_ok());
        assert!(check_viewport(0, 10).is_err());
        assert!(check_viewport(4000, 4000).is_err());
    }

    #[test]
    fn reload_state_maps_details_and_focus_by_author_id() {
        use blitz_dom::DocumentConfig;
        use blitz_html::HtmlDocument;

        let html = concat!(
            "<!doctype html><style>body{font:16px monospace}</style>",
            "<a id=\"jump\" href=\"#target\">Jump</a>",
            "<details id=\"evidence\"><summary>Evidence</summary><p id=\"target\">Body</p></details>"
        );
        let mut document = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(text_viewport()),
                ..Default::default()
            },
        )
        .into_inner();
        document.resolve(0.0);
        let mut view = DocViewport::new(document, TEXT_VIEWPORT_WIDTH, TEXT_VIEWPORT_HEIGHT, 1.0);
        let jump = view
            .document()
            .get_element_by_id("jump")
            .expect("fixture link");
        view.document_mut().set_focus_to(jump);
        let details = view
            .document()
            .get_element_by_id("evidence")
            .expect("fixture details");
        view.document_mut().toggle_details_open(details);
        view.resolve();
        let state = capture_reload_state(&mut view, &Search::new());
        assert_eq!(state.open_details, vec!["evidence".to_string()]);

        // Same author ids in a fresh document produce fresh DOM-local NodeIds.
        let mut document = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(text_viewport()),
                ..Default::default()
            },
        )
        .into_inner();
        document.resolve(0.0);
        let mut candidate =
            DocViewport::new(document, TEXT_VIEWPORT_WIDTH, TEXT_VIEWPORT_HEIGHT, 1.0);
        restore_open_details(&mut candidate, &state.open_details);
        restore_focus(&mut candidate, state.focus.as_ref());

        let details = candidate
            .document()
            .get_element_by_id("evidence")
            .expect("reloaded details");
        let open = candidate
            .document()
            .get_node(details)
            .is_some_and(|node| node.data.has_attr(local_name!("open")));
        assert!(open, "disclosure id did not retain its open state");
        let focus = candidate
            .document()
            .get_focussed_node_id()
            .and_then(|node| viewport::author_id(candidate.document(), node));
        assert_eq!(focus.as_deref(), Some("jump"));
    }
}
