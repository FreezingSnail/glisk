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
mod search;
mod terminal;
mod text;
mod tmux;
mod viewport;

use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

use anyrender_vello_cpu::VelloCpuScenePainter;
use blitz_dom::{BaseDocument, ScrollBehavior, ScrollLogicalPosition};
use blitz_traits::shell::{ColorScheme, Viewport as BlitzViewport};
use clap::Parser;

use document::{IngestError, IngestedDocument, Limits};
use navigation::Navigation;
use search::Search;
use terminal::{Geometry, TerminalConfig, TerminalError, TerminalOwner};
use viewport::{PaintOutcome, ResizeOutcome, Viewport as DocViewport};

const MAX_RASTER_PIXELS: u64 = 8_388_608;
const TEXT_VIEWPORT_WIDTH: u32 = 420;
const TEXT_VIEWPORT_HEIGHT: u32 = 900;
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(100);
const SCROLL_STEP: f64 = 40.0;

#[derive(Parser, Debug)]
#[command(name = "glisk", about = "Glisk HTML pager", version)]
struct Args {
    /// Local .htm/.html file, or `-` to read the document from stdin.
    input: String,

    /// Write plain UTF-8 text instead of opening the live pager.
    #[arg(long)]
    text: bool,

    /// Open the live pager with animations paused.
    #[arg(long)]
    pause_animation: bool,

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
    // Ingest with a deterministic text viewport first. The live path re-lays
    // out the same live document at terminal geometry after the owner opens.
    let ingested = ingest_source(&args.input, text_viewport())?;

    if args.text || !terminal::is_tty(libc::STDOUT_FILENO) {
        return write_plain_text(&ingested.document);
    }

    match TerminalOwner::open_with_config(TerminalConfig::default()) {
        Ok(owner) => run_pager(args, ingested, owner, started),
        Err(err) => {
            eprintln!("glisk: graphics unavailable: {err}; using plain text");
            write_plain_text(&ingested.document)
        }
    }
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
        format!(
            "glisk | {percent:.0}%{animation_status} | {} | {} | Tab focus  Enter open  / search  j/k scroll  q quit",
            self.search.status(),
            self.navigation.status()
        )
    }

    fn run(
        &mut self,
        args: &Args,
        owner: &mut TerminalOwner,
        started: Instant,
    ) -> Result<(), String> {
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
            self.metrics.write(args.metrics.as_deref())?;
            return Ok(());
        }

        if args.once {
            let _ = io::stdout().flush();
            std::thread::sleep(Duration::from_millis(700));
            self.metrics.push("frames", &self.frames.to_string());
            note_rss(&mut self.metrics);
            self.metrics.write(args.metrics.as_deref())?;
            return Ok(());
        }

        let mut last_sample = Instant::now();
        let mut last_frame = last_sample;
        loop {
            let timeout = if self.animation_active(owner) {
                ANIMATION_FRAME_INTERVAL.saturating_sub(last_frame.elapsed())
            } else {
                Duration::from_millis(100)
            };
            let batch = owner.poll_input(timeout).map_err(terminal_message)?;
            let mut changed = false;
            let mut quit = false;
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
                        changed |= self.handle_input(input, owner.geometry(), &mut quit);
                    }
                }
            }
            if quit {
                break;
            }
            if owner.take_repaint_request() {
                self.metrics.count("tmux_repaints");
                changed = true;
            }

            if changed
                || (self.animation_active(owner)
                    && last_frame.elapsed() >= ANIMATION_FRAME_INTERVAL)
            {
                self.draw(owner, args)?;
                last_frame = Instant::now();
            }
        }

        self.metrics.push("frames", &self.frames.to_string());
        note_rss(&mut self.metrics);
        self.metrics.write(args.metrics.as_deref())
    }

    fn animation_active(&self, owner: &TerminalOwner) -> bool {
        !self.animation_paused
            && self.view.geometry().is_some()
            && owner.tmux_visible() != Some(false)
            && self.view.is_animating()
    }

    fn handle_input(&mut self, input: Input, geometry: Geometry, quit: &mut bool) -> bool {
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

fn run_pager(
    args: &Args,
    ingested: IngestedDocument,
    mut owner: TerminalOwner,
    started: Instant,
) -> Result<(), String> {
    let geometry = owner.geometry();
    let (width, height) = geometry.physical_size();
    let mut pager = Pager::new(ingested.document, width, height, geometry.scale());
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

    if let Err(err) = pager.prepare(args) {
        let _ = owner.finish();
        return Err(err);
    }

    let result = pager.run(args, &mut owner, started);
    let finish = owner.finish().map_err(terminal_message);
    match (result, finish) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(err),
        (Ok(()), Ok(())) => Ok(()),
    }
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
        &args.input,
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
        &args.input,
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
        &args.input,
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
}
