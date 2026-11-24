//! Minimal live renderer spike for bead glisk-2hp.1.
//!
//! Loads one self-contained HTML document with external resources denied,
//! resolves layout with Blitz, rasterizes a viewport with Vello CPU, and
//! presents one Kitty graphics placement on the alternate screen.
//!
//! `snapshot` mode is headless and writes a PNG. `run` mode is interactive.

use std::{
    io::{Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

use anyrender_vello_cpu::VelloCpuScenePainter;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use blitz_dom::{BaseDocument, DocumentConfig, ScrollBehavior, ScrollLogicalPosition};
use blitz_html::HtmlDocument;
use blitz_traits::net::{NetHandler, Request};
use blitz_traits::shell::{ColorScheme, Viewport};
use clap::Parser;
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

const IMAGE_ID: u32 = 0x474c_4953; // "GLIS"
const STATUS_ROWS: u16 = 1;
const CSS_PX_PER_COLUMN: f64 = 8.0;

#[derive(Parser, Debug)]
#[command(about = "Glisk Blitz + Kitty renderer spike", version)]
struct Args {
    /// Self-contained HTML document.
    file: String,

    /// Headless mode: write one viewport PNG to this path.
    #[arg(long)]
    snapshot: Option<String>,

    /// CSS width for --snapshot (default 420).
    #[arg(long, default_value_t = 420.0)]
    css_width: f64,

    /// CSS height for --snapshot (default 900).
    #[arg(long, default_value_t = 900.0)]
    css_height: f64,

    /// Device scale for --snapshot (default 2.0).
    #[arg(long, default_value_t = 2.0)]
    scale: f64,

    /// Scroll to an element id before painting / opening.
    #[arg(long)]
    fragment: Option<String>,

    /// Toggle a <details> element id before painting / opening.
    #[arg(long)]
    toggle: Option<String>,

    /// Focus the first node matching this CSS selector before painting.
    #[arg(long)]
    focus: Option<String>,

    /// Open, draw once, then exit (useful for screenshots).
    #[arg(long)]
    once: bool,

    /// After the first frame, run a fixed action sequence through the live
    /// handlers and record action-to-frame-write timings. This is not OS
    /// keyboard injection; the terminal keys are not exercised.
    #[arg(long)]
    scripted_actions: bool,

    /// Cell width in device pixels when the terminal does not answer CSI 16t.
    #[arg(long)]
    cell_width_px: Option<u16>,

    /// Cell height in device pixels when the terminal does not answer CSI 16t.
    #[arg(long)]
    cell_height_px: Option<u16>,

    /// Run the headless interaction and resize state assertions.
    #[arg(long)]
    self_test: bool,

    /// Write every live PNG frame into this directory.
    #[arg(long)]
    dump_dir: Option<String>,

    /// Write key=value metrics here when the process exits.
    #[arg(long)]
    metrics: Option<String>,
}

fn main() {
    let started = Instant::now();
    let args = Args::parse();
    let html = match std::fs::read_to_string(&args.file) {
        Ok(html) => html,
        Err(err) => {
            eprintln!("glisk-spike: cannot read {}: {err}", args.file);
            std::process::exit(1);
        }
    };

    let result = if args.self_test {
        self_test(&args, &html, started)
    } else if let Some(path) = &args.snapshot {
        snapshot(&args, &html, path, started)
    } else {
        live(&args, &html, started)
    };

    if let Err(err) = result {
        eprintln!("glisk-spike: {err}");
        std::process::exit(1);
    }
}

fn self_test(args: &Args, html: &str, started: Instant) -> Result<(), String> {
    use blitz_dom::{ScrollBehavior, ScrollLogicalPosition, local_name};

    let mut metrics = Metrics::new();
    metrics.push("mode", "self-test");

    let narrow = Geometry::from_parts(52, 57, 16, 32); // 420x1792 CSS-equivalent at 2x
    let wide = Geometry::from_parts(125, 57, 16, 32); // 1000x1792 CSS-equivalent at 2x
    let mut doc = load(html, narrow.phys_w, narrow.phys_h, narrow.scale as f32)?;

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

    doc.set_viewport(Viewport::new(
        wide.phys_w,
        wide.phys_h,
        wide.scale as f32,
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
    metrics.push("resize_css_width", &format!("{:.1}", wide.css_w));
    metrics.push("resize_css_height", &format!("{:.1}", wide.css_h));
    metrics.push("cold_start_to_self_test_ms", &format_ms(started.elapsed()));
    metrics.push("result", "pass");
    note_rss(&mut metrics);
    metrics.write(args.metrics.as_deref())?;
    eprintln!("glisk-spike: self-test passed");
    Ok(())
}

fn snapshot(args: &Args, html: &str, path: &str, started: Instant) -> Result<(), String> {
    let width = (args.css_width * args.scale).round() as u32;
    let height = (args.css_height * args.scale).round() as u32;
    check_viewport(width, height)?;
    let mut doc = load(html, width, height, args.scale as f32)?;
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
        "glisk-spike: snapshot {width}x{height}px {} bytes in {:.1} ms -> {path}",
        png.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}

fn live(args: &Args, html: &str, started: Instant) -> Result<(), String> {
    let mut session = Session::enter()?;
    let mut input = Input::new();
    let mut geo = Geometry::new(args, session.size()?, &mut input);
    let mut doc = load(html, geo.phys_w, geo.phys_h, geo.scale as f32)?;
    prepare(&mut doc, args)?;

    let mut metrics = Metrics::new();
    metrics.push("mode", "live");
    metrics.push(
        "term_program",
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
    );
    metrics.push("term", &std::env::var("TERM").unwrap_or_default());
    metrics.push("cell_width_px", &geo.cell_w.to_string());
    metrics.push("cell_height_px", &geo.cell_h.to_string());
    metrics.push("cols", &geo.cols.to_string());
    metrics.push("rows", &geo.rows.to_string());
    metrics.push("css_width", &format!("{:.1}", geo.css_w));
    metrics.push("css_height", &format!("{:.1}", geo.css_h));
    metrics.push("scale", &format!("{:.3}", geo.scale));

    let mut key_durations = Vec::new();
    let mut frame_count = 0u32;
    let mut last_draw = draw(
        &mut doc,
        &geo,
        &mut session,
        &mut frame_count,
        args.dump_dir.as_deref(),
    )?;
    let first = started.elapsed();
    metrics.push("cold_start_to_frame_write_ms", &format_ms(first));
    metrics.push("png_bytes_first_frame", &last_draw.0.to_string());
    metrics.push("frame_bytes_first_frame", &last_draw.1.to_string());

    if args.scripted_actions {
        let script = [
            Action::FocusNext,
            Action::Activate,
            Action::FocusNext,
            Action::Activate,
            Action::Anchor("chart".to_string()),
            Action::Scroll(40.0),
            Action::Scroll(-40.0),
            Action::Anchor("evidence".to_string()),
            Action::Toggle("evidence-method".to_string()),
        ];
        for action in script {
            let at = Instant::now();
            apply_action(&mut doc, action);
            doc.resolve(0.0);
            last_draw = draw(
                &mut doc,
                &geo,
                &mut session,
                &mut frame_count,
                args.dump_dir.as_deref(),
            )?;
            session.status(&doc, &geo)?;
            key_durations.push(at.elapsed());
        }
        let avg: f64 = key_durations
            .iter()
            .map(|d| d.as_secs_f64() * 1000.0)
            .sum::<f64>()
            / key_durations.len() as f64;
        metrics.push("scripted_action_frames", &key_durations.len().to_string());
        metrics.push(
            "scripted_action_to_frame_write_last_ms",
            &format_ms(*key_durations.last().unwrap()),
        );
        metrics.push(
            "scripted_action_to_frame_write_avg_ms",
            &format!("{avg:.1}"),
        );
        metrics.push("png_bytes_last", &last_draw.0.to_string());
        metrics.push("frame_bytes_last", &last_draw.1.to_string());
        metrics.push("frames", &frame_count.to_string());
        note_rss(&mut metrics);
        metrics.write(args.metrics.as_deref())?;
        return Ok(());
    }

    if args.once {
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(700));
        metrics.push("frames", &frame_count.to_string());
        note_rss(&mut metrics);
        metrics.write(args.metrics.as_deref())?;
        return Ok(());
    }

    loop {
        let wait_key = Instant::now();
        let actions = input.next_actions(100);
        let key_at = wait_key.elapsed();

        if actions.is_empty() {
            if input.resized(&mut geo, session.size()?) {
                geo.apply(&mut doc);
                last_draw = draw(
                    &mut doc,
                    &geo,
                    &mut session,
                    &mut frame_count,
                    args.dump_dir.as_deref(),
                )?;
                session.status(&doc, &geo)?;
                metrics.push("last_resize_frame", &frame_count.to_string());
            }
            continue;
        }

        for action in actions {
            if matches!(action, Action::Quit) {
                metrics.push("frames", &frame_count.to_string());
                metrics.push(
                    "key_to_frame_write_last_ms",
                    &format_ms(*key_durations.last().unwrap_or(&Duration::ZERO)),
                );
                if !key_durations.is_empty() {
                    let avg: f64 = key_durations
                        .iter()
                        .map(|d| d.as_secs_f64() * 1000.0)
                        .sum::<f64>()
                        / key_durations.len() as f64;
                    metrics.push("key_to_frame_write_avg_ms", &format!("{avg:.1}"));
                }
                metrics.push("key_frames", &key_durations.len().to_string());
                metrics.push("png_bytes_last", &last_draw.0.to_string());
                metrics.push("frame_bytes_last", &last_draw.1.to_string());
                note_rss(&mut metrics);
                metrics.write(args.metrics.as_deref())?;
                return Ok(());
            }
            apply_action(&mut doc, action);
        }

        doc.resolve(0.0);
        last_draw = draw(
            &mut doc,
            &geo,
            &mut session,
            &mut frame_count,
            args.dump_dir.as_deref(),
        )?;
        session.status(&doc, &geo)?;
        key_durations.push(key_at);
        if key_durations.len() > 200 {
            key_durations.remove(0);
        }
    }
}

fn draw(
    doc: &mut BaseDocument,
    geo: &Geometry,
    session: &mut Session,
    frame_count: &mut u32,
    dump_dir: Option<&str>,
) -> Result<(usize, usize), String> {
    let png = render(doc, geo.phys_w, geo.phys_h, geo.scale)?;
    *frame_count += 1;
    if let Some(dir) = dump_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
        let path = format!("{dir}/frame-{frame_count:03}.png");
        std::fs::write(&path, &png).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    let frame_bytes = session.present(&png, geo.cols, geo.rows - STATUS_ROWS)?;
    Ok((png.len(), frame_bytes))
}

fn apply_action(doc: &mut BaseDocument, action: Action) {
    match action {
        Action::Scroll(delta) => {
            doc.scroll_viewport_by_has_changed(0.0, -delta);
        }
        Action::FocusNext => {
            let next = doc
                .get_focussed_node_id()
                .and_then(|_| doc.focus_next_node())
                .or_else(|| first_focusable(doc).filter(|id| doc.set_focus_to(*id)));
            if let Some(id) = next {
                doc.scroll_into_view(
                    id,
                    ScrollBehavior::Instant,
                    ScrollLogicalPosition::Nearest,
                    ScrollLogicalPosition::Nearest,
                );
            }
        }
        Action::FocusPrev => {
            let prev = doc
                .get_focussed_node_id()
                .and_then(|_| doc.focus_prev_node())
                .or_else(|| first_focusable(doc).filter(|id| doc.set_focus_to(*id)));
            if let Some(id) = prev {
                doc.scroll_into_view(
                    id,
                    ScrollBehavior::Instant,
                    ScrollLogicalPosition::Nearest,
                    ScrollLogicalPosition::Nearest,
                );
            }
        }
        Action::Activate => activate(doc),
        Action::Home => doc.set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 }),
        Action::End => {
            doc.scroll_viewport_by_has_changed(0.0, -1.0e9);
        }
        Action::Anchor(id) => {
            doc.scroll_to_fragment(&id);
        }
        Action::Toggle(id) => {
            if let Some(node) = doc.get_element_by_id(&id) {
                doc.toggle_details_open(node);
            }
        }
        Action::Quit => {}
    }
}

fn load(html: &str, phys_w: u32, phys_h: u32, scale: f32) -> Result<BaseDocument, String> {
    check_viewport(phys_w, phys_h)?;
    let config = DocumentConfig {
        viewport: Some(Viewport::new(phys_w, phys_h, scale, ColorScheme::Dark)),
        base_url: Some("glisk://report/reference.html".to_string()),
        net_provider: Some(Arc::new(DenyNetProvider)),
        html_parser_provider: None,
        ..Default::default()
    };
    let mut doc = HtmlDocument::from_html(html, config).into_inner();
    doc.resolve(0.0);
    Ok(doc)
}

fn check_viewport(w: u32, h: u32) -> Result<(), String> {
    const MAX_PIXELS: u64 = 8_388_608;
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(format!("invalid viewport {w}x{h}"));
    }
    if w as u64 * h as u64 > MAX_PIXELS {
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

fn first_focusable(doc: &BaseDocument) -> Option<blitz_dom::NodeId> {
    let mut id = doc.root_node().id;
    for _ in 0..20_000 {
        let node = doc.get_node(id)?;
        if node.is_focussable() {
            return Some(id);
        }
        id = doc.next_node(node, |_| true)?;
    }
    None
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

struct Geometry {
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
    phys_w: u32,
    phys_h: u32,
    css_w: f64,
    css_h: f64,
    scale: f64,
}

impl Geometry {
    fn new(args: &Args, (cols, rows): (u16, u16), input: &mut Input) -> Self {
        let (cell_w, cell_h) = input
            .probe_cell_pixels()
            .or(match (args.cell_width_px, args.cell_height_px) {
                (Some(w), Some(h)) => Some((w, h)),
                _ => None,
            })
            .unwrap_or((8, 16));
        Self::from_parts(cols, rows, cell_w, cell_h)
    }

    fn from_parts(cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let cell_w = cell_w.max(1);
        let cell_h = cell_h.max(1);
        let phys_w = cols as u32 * cell_w as u32;
        let phys_h = (rows as u32 - STATUS_ROWS as u32) * cell_h as u32;
        let scale = cell_w as f64 / CSS_PX_PER_COLUMN;
        let css_w = phys_w as f64 / scale;
        let css_h = phys_h as f64 / scale;
        Self {
            cols,
            rows,
            cell_w,
            cell_h,
            phys_w,
            phys_h,
            css_w,
            css_h,
            scale,
        }
    }

    fn apply(&self, doc: &mut BaseDocument) {
        doc.set_viewport(Viewport::new(
            self.phys_w,
            self.phys_h,
            self.scale as f32,
            ColorScheme::Dark,
        ));
        doc.resolve(0.0);
    }
}

struct Session {
    stdout: std::io::Stdout,
}

impl Session {
    fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
        let mut stdout = std::io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(format!("alternate screen: {err}"));
        }
        Ok(Self { stdout })
    }

    fn size(&self) -> Result<(u16, u16), String> {
        crossterm::terminal::size().map_err(|e| format!("terminal size: {e}"))
    }

    fn present(&mut self, png: &[u8], cols: u16, rows: u16) -> Result<usize, String> {
        let frame = kitty_frame(png, cols, rows);
        let len = frame.len();
        self.stdout
            .write_all(&frame)
            .and_then(|_| self.stdout.flush())
            .map_err(|e| format!("frame write: {e}"))?;
        Ok(len)
    }

    fn status(&mut self, doc: &BaseDocument, geo: &Geometry) -> Result<(), String> {
        let focus = doc
            .get_focussed_node_id()
            .and_then(|id| doc.get_node(id))
            .and_then(|n| n.element_data())
            .and_then(|e| e.id.as_ref().map(|id| id.to_string()))
            .unwrap_or_else(|| "-".to_string());
        let mut text = format!(
            "glisk-spike | {}x{} cells | {}x{}px | {:.1}x{:.1} CSS | scale {:.2} | focus {} | Tab focus  Enter open  j/k scroll  q quit",
            geo.cols, geo.rows, geo.cell_w, geo.cell_h, geo.css_w, geo.css_h, geo.scale, focus
        );
        let max = geo.cols as usize;
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
        }
        let seq = format!("\x1b[{};1H\x1b[2K{}", geo.rows, text);
        self.stdout
            .write_all(seq.as_bytes())
            .and_then(|_| self.stdout.flush())
            .map_err(|e| format!("status write: {e}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let delete = format!("\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\");
        let _ = self.stdout.write_all(delete.as_bytes());
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = self.stdout.flush();
    }
}

fn kitty_frame(png: &[u8], cols: u16, rows: u16) -> Vec<u8> {
    let encoded = BASE64.encode(png);
    let mut out = Vec::with_capacity(encoded.len() + 128);
    out.extend_from_slice(b"\x1b[H");
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(4000).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = usize::from(i + 1 < chunks.len());
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,f=100,i={IMAGE_ID},q=2,C=1,c={cols},r={rows},m={more};")
                    .as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session")
    }
}

/// Deny every document-directed resource. The document is fully inline.
#[derive(Debug)]
struct DenyNetProvider;

impl blitz_traits::net::NetProvider for DenyNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, _handler: Box<dyn NetHandler>) {
        eprintln!("glisk-spike: blocked resource {}", request.url);
    }
    fn is_noop(&self) -> bool {
        true
    }
}

struct Input {
    pending: Vec<u8>,
}

impl Input {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn probe_cell_pixels(&mut self) -> Option<(u16, u16)> {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[16t");
        let _ = stdout.flush();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            let bytes = self.read_bytes(50);
            if bytes.is_empty() {
                continue;
            }
            self.pending.extend_from_slice(&bytes);
            let (rest, parsed) = parse_state(&self.pending, true);
            self.pending = rest;
            if parsed.cell.is_some() {
                return parsed.cell;
            }
        }
        None
    }

    fn next_actions(&mut self, timeout_ms: i32) -> Vec<Action> {
        let wait = if self.pending.is_empty() {
            timeout_ms
        } else {
            0
        };
        let bytes = self.read_bytes(wait);
        self.pending.extend_from_slice(&bytes);
        let (rest, parsed) = parse_state(&self.pending, false);
        self.pending = rest;
        parsed.actions
    }

    fn resized(&self, geo: &mut Geometry, (cols, rows): (u16, u16)) -> bool {
        if cols == geo.cols && rows == geo.rows {
            return false;
        }
        *geo = Geometry::from_parts(cols, rows, geo.cell_w, geo.cell_h);
        true
    }

    fn read_bytes(&mut self, timeout_ms: i32) -> Vec<u8> {
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
        if ready <= 0 {
            return Vec::new();
        }
        let mut buf = [0u8; 512];
        match std::io::stdin().read(&mut buf) {
            Ok(n) => buf[..n].to_vec(),
            Err(_) => Vec::new(),
        }
    }
}

#[derive(Default)]
struct Parsed {
    actions: Vec<Action>,
    cell: Option<(u16, u16)>,
}

enum Action {
    Quit,
    Scroll(f64),
    FocusNext,
    FocusPrev,
    Activate,
    Home,
    End,
    Anchor(String),
    Toggle(String),
}

/// Parse as many complete input sequences as possible. Returns unparsed tail.
fn parse_state(bytes: &[u8], probe_only: bool) -> (Vec<u8>, Parsed) {
    let mut parsed = Parsed::default();
    let mut i = 0;
    let mut tail_start = bytes.len();
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            if i + 1 >= bytes.len() {
                tail_start = i;
                break;
            }
            if bytes[i + 1] == b'[' {
                let start = i + 2;
                let mut end = None;
                let mut j = start;
                while j < bytes.len() {
                    if (0x40..=0x7e).contains(&bytes[j]) {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                let Some(end) = end else {
                    tail_start = i;
                    break;
                };
                let params = &bytes[start..end];
                let fin = bytes[end];
                match (params, fin) {
                    (_, b't') if params.first() == Some(&b'6') => {
                        let parts: Vec<&[u8]> = params.split(|b| *b == b';').collect();
                        if parts.len() == 3
                            && let (Ok(h), Ok(w)) = (
                                std::str::from_utf8(parts[1]).unwrap_or("").parse::<u16>(),
                                std::str::from_utf8(parts[2]).unwrap_or("").parse::<u16>(),
                            )
                            && w > 0
                            && h > 0
                        {
                            parsed.cell = Some((w, h));
                        }
                    }
                    (_, b'Z') => parsed.actions.push(Action::FocusPrev),
                    (_, b'A') => parsed.actions.push(Action::Scroll(-40.0)),
                    (_, b'B') => parsed.actions.push(Action::Scroll(40.0)),
                    (params, b'~') => match params {
                        b"5" => parsed.actions.push(Action::Scroll(-400.0)),
                        b"6" => parsed.actions.push(Action::Scroll(400.0)),
                        _ => {}
                    },
                    _ => {}
                }
                i = end + 1;
                continue;
            }
            if bytes[i + 1] == b'O' {
                if i + 2 >= bytes.len() {
                    tail_start = i;
                    break;
                }
                match bytes[i + 2] {
                    b'A' => parsed.actions.push(Action::Scroll(-40.0)),
                    b'B' => parsed.actions.push(Action::Scroll(40.0)),
                    _ => {}
                }
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        if !probe_only {
            match b {
                b'q' | 0x03 => parsed.actions.push(Action::Quit),
                b'\t' => parsed.actions.push(Action::FocusNext),
                b'\r' | b'\n' => parsed.actions.push(Action::Activate),
                b'j' => parsed.actions.push(Action::Scroll(40.0)),
                b'k' => parsed.actions.push(Action::Scroll(-40.0)),
                b' ' => parsed.actions.push(Action::Scroll(400.0)),
                b'g' => parsed.actions.push(Action::Home),
                b'G' => parsed.actions.push(Action::End),
                b'1'..=b'9' => {
                    parsed
                        .actions
                        .push(Action::Anchor(format!("#{}", (b - b'0'))));
                }
                _ => {}
            }
        }
        i += 1;
    }
    (bytes[tail_start..].to_vec(), parsed)
}

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
    fn write(&self, path: Option<&str>) -> Result<(), String> {
        let Some(path) = path else { return Ok(()) };
        let body: String = self
            .lines
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        std::fs::write(path, body).map_err(|e| format!("cannot write {path}: {e}"))
    }
}

fn format_ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
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
    fn kitty_frame_sets_placement_and_chunks() {
        let png = vec![0u8; 9000];
        let frame = kitty_frame(&png, 46, 16);
        let text = String::from_utf8_lossy(&frame);
        assert!(text.starts_with("\x1b[H\x1b_Ga=T,f=100,i="));
        assert!(text.contains(",c=46,r=16,m=1;"));
        assert!(text.contains("\x1b_Gm=0;"));
        assert!(text.ends_with("\x1b\\"));
    }

    #[test]
    fn parses_cell_size_reply_without_losing_keys() {
        let bytes = b"\x1b[6;34;17t\x1b[Zj";
        let (rest, parsed) = parse_state(bytes, false);
        assert_eq!(parsed.cell, Some((17, 34)));
        assert_eq!(parsed.actions.len(), 2);
        assert!(rest.is_empty());
    }

    #[test]
    fn inline_document_with_external_asset_still_renders() {
        let html = r##"<html><body style="background:#000;color:#fff">
            <img src="https://example.invalid/x.png" alt="blocked">
            <a href="#target">jump</a><div id="target">target</div>
            <details><summary>one</summary><p>body</p></details>
        </body></html>"##;
        let mut doc = load(html, 400, 300, 1.0).expect("load");
        let png = render(&mut doc, 400, 300, 1.0).expect("render");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
