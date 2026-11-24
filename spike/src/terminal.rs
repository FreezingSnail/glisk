//! Terminal owner for the Glisk live pager (bead `glisk-2hp.3`).
//!
//! One owner is responsible for:
//!
//! - confirming that the input and output file descriptors refer to the same TTY;
//! - a bounded, shared-budget Ghostty/Kitty graphics + cell-pixel probe;
//! - raw mode and the alternate screen;
//! - retaining ordinary key bytes typed while probe replies are parsed;
//! - transmitting a viewport PNG as one Kitty image with a stable image ID,
//!   explicit cell geometry, cursor preservation, and bounded chunking;
//! - painting one reserved status row above/below that geometry;
//! - recomputing geometry on resize and validating every dimension;
//! - removing the owned image and restoring saved terminal modes on normal quit,
//!   interrupt (`0x03` read in raw mode), and every handled error.
//!
//! The module is deliberately not wired into `main.rs` yet: bead `glisk-2hp.8`
//! owns that integration. The API and ordering contract are documented in
//! `docs/terminal-session.md`.
//!
//! Startup order is fixed: the caller validates/loads the document first, then
//! calls [`TerminalOwner::open`]. Probing happens before the alternate screen is
//! entered, so a failed probe leaves the normal screen untouched and maps to the
//! caller's plain-text path. The returned error distinguishes timeout,
//! unsupported graphics, unexpected replies, missing geometry, and incomplete
//! replies.

use std::{
    env, fmt,
    fs::{File, OpenOptions},
    io,
    os::fd::{AsRawFd, RawFd},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// Stable image id (`"GLIS"`). Every frame replaces this one image.
pub const DEFAULT_IMAGE_ID: u32 = 0x474c_4953;
/// Shared budget for both probe queries, matching the feasibility note.
pub const DEFAULT_PROBE_BUDGET: Duration = Duration::from_millis(250);
/// One terminal row is reserved for search/status text.
pub const DEFAULT_STATUS_ROWS: u16 = 1;
/// Starting CSS width of one terminal column; the device scale is derived from
/// the measured cell width.
pub const DEFAULT_CSS_PX_PER_COLUMN: f64 = 8.0;
/// Base64 payload bytes per Kitty chunk.
pub const DEFAULT_CHUNK_BYTES: usize = 4000;
/// Kitty protocol chunk payload ceiling.
pub const MAX_CHUNK_BYTES: usize = 4096;
/// Provisional viewport raster budget from `FEASIBILITY.md`.
pub const MAX_RASTER_PIXELS: u32 = 8_388_608;
/// Provisional encoded PNG budget from `FEASIBILITY.md`.
pub const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
/// Bound on the base64 frame before Kitty framing.
pub const MAX_ENCODED_BYTES: usize = 4 * 1024 * 1024;

const PROBE_IMAGE_ID: u32 = 31;
const MAX_CELL_PX: u16 = 4096;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const SAVE_CURSOR: &[u8] = b"\x1b7";
const RESTORE_CURSOR: &[u8] = b"\x1b8";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Deterministic probe outcomes. All of these select the caller's plain-text
/// path; they never fall back to guessed cell geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailureKind {
    /// No required reply arrived inside the shared budget.
    Timeout,
    /// The graphics query got an explicit negative reply, or no graphics reply
    /// while the cell query was answered.
    UnsupportedGraphics,
    /// A reply was present but malformed, for a foreign image id, or otherwise
    /// not a protocol reply this owner accepts.
    UnexpectedReply,
    /// Graphics support was acknowledged but no usable cell size was reported.
    MissingGeometry,
    /// The deadline arrived with a partial escape sequence still buffered.
    IncompleteReply,
}

impl ProbeFailureKind {
    /// Stable diagnostic label, useful for metrics and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::UnsupportedGraphics => "unsupported-graphics",
            Self::UnexpectedReply => "unexpected-reply",
            Self::MissingGeometry => "missing-geometry",
            Self::IncompleteReply => "incomplete-reply",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    pub kind: ProbeFailureKind,
    pub detail: String,
    pub elapsed: Duration,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal probe {}: {}", self.kind.as_str(), self.detail)
    }
}

#[derive(Debug, Clone)]
pub struct GeometryError {
    pub detail: String,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal geometry: {}", self.detail)
    }
}

#[derive(Debug)]
pub enum TerminalError {
    Config(String),
    NotATerminal(&'static str),
    NotSameTerminal,
    TtyOpen(String),
    RawMode(String),
    Size(String),
    Probe(ProbeError),
    Geometry(GeometryError),
    InvalidPng,
    FrameTooLarge {
        png_bytes: usize,
        encoded_bytes: usize,
        limit: usize,
    },
    InputClosed,
    Finished,
    Io {
        op: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(detail) => write!(f, "terminal config: {detail}"),
            Self::NotATerminal(which) => write!(f, "{which} is not a terminal"),
            Self::NotSameTerminal => {
                write!(f, "input and output refer to different terminals")
            }
            Self::TtyOpen(detail) => write!(f, "cannot open controlling terminal: {detail}"),
            Self::RawMode(detail) => write!(f, "raw mode: {detail}"),
            Self::Size(detail) => write!(f, "terminal size: {detail}"),
            Self::Probe(err) => write!(f, "{err}"),
            Self::Geometry(err) => write!(f, "{err}"),
            Self::InvalidPng => write!(f, "frame is not a PNG"),
            Self::FrameTooLarge {
                png_bytes,
                encoded_bytes,
                limit,
            } => write!(
                f,
                "frame too large: {png_bytes} PNG bytes, {encoded_bytes} encoded bytes, limit {limit}"
            ),
            Self::InputClosed => write!(f, "terminal input closed"),
            Self::Finished => write!(f, "terminal session already finished"),
            Self::Io { op, source } => write!(f, "{op}: {source}"),
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<GeometryError> for TerminalError {
    fn from(value: GeometryError) -> Self {
        Self::Geometry(value)
    }
}

// ---------------------------------------------------------------------------
// Environment hints (diagnostics only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsHint {
    Ghostty,
    Kitty,
    WezTerm,
    Other(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentHint {
    pub term_program: Option<String>,
    pub term: Option<String>,
    pub kitty_window: bool,
    pub graphics: GraphicsHint,
}

impl EnvironmentHint {
    /// Read environment variables. These are hints for diagnostics only; they
    /// never decide the mode or supply geometry.
    pub fn detect() -> Self {
        let term_program = env::var("TERM_PROGRAM").ok();
        let term = env::var("TERM").ok();
        let kitty_window = env::var_os("KITTY_WINDOW_ID").is_some();
        let graphics = match (term_program.as_deref(), term.as_deref()) {
            (Some(p), _) if p.eq_ignore_ascii_case("ghostty") => GraphicsHint::Ghostty,
            (Some(p), _) if p.eq_ignore_ascii_case("wezterm") => GraphicsHint::WezTerm,
            (_, Some(t)) if t.contains("kitty") || kitty_window => GraphicsHint::Kitty,
            (Some(p), _) if !p.is_empty() => GraphicsHint::Other(p.to_string()),
            (_, Some(t)) if !t.is_empty() => GraphicsHint::Other(t.to_string()),
            _ => GraphicsHint::Unknown,
        };
        Self {
            term_program,
            term,
            kitty_window,
            graphics,
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPixels {
    width: u16,
    height: u16,
}

impl CellPixels {
    /// Cell pixels must be positive and plausible. Zero or implausible values
    /// are a probe failure, not something to guess around.
    pub fn new(width: u16, height: u16) -> Option<Self> {
        if (1..=MAX_CELL_PX).contains(&width) && (1..=MAX_CELL_PX).contains(&height) {
            Some(Self { width, height })
        } else {
            None
        }
    }

    pub fn width(self) -> u16 {
        self.width
    }

    pub fn height(self) -> u16 {
        self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    cols: u16,
    rows: u16,
    content_rows: u16,
    cell: CellPixels,
    physical_w: u32,
    physical_h: u32,
    css_w: f64,
    css_h: f64,
    scale: f64,
    status_rows: u16,
}

impl Geometry {
    pub fn compute(
        cols: u16,
        rows: u16,
        cell: CellPixels,
        status_rows: u16,
    ) -> Result<Self, GeometryError> {
        Self::compute_with_cap(cols, rows, cell, status_rows, MAX_RASTER_PIXELS)
    }

    pub fn compute_with_cap(
        cols: u16,
        rows: u16,
        cell: CellPixels,
        status_rows: u16,
        max_raster_pixels: u32,
    ) -> Result<Self, GeometryError> {
        if status_rows == 0 {
            return Err(GeometryError {
                detail: "status row count must be positive".to_string(),
            });
        }
        if cols < 2 {
            return Err(GeometryError {
                detail: format!("too few columns: {cols}"),
            });
        }
        if rows <= status_rows {
            return Err(GeometryError {
                detail: format!(
                    "no content rows: {rows} total rows with {status_rows} status rows"
                ),
            });
        }
        let content_rows = rows - status_rows;
        let physical_w = u32::from(cols) * u32::from(cell.width);
        let physical_h = u32::from(content_rows) * u32::from(cell.height);
        let pixels = u64::from(physical_w) * u64::from(physical_h);
        if pixels > u64::from(max_raster_pixels) {
            return Err(GeometryError {
                detail: format!(
                    "raster {physical_w}x{physical_h} exceeds {max_raster_pixels} pixels"
                ),
            });
        }
        let scale = f64::from(cell.width) / DEFAULT_CSS_PX_PER_COLUMN;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(GeometryError {
                detail: "invalid device scale".to_string(),
            });
        }
        let css_w = f64::from(physical_w) / scale;
        let css_h = f64::from(physical_h) / scale;
        if !css_w.is_finite() || !css_h.is_finite() || css_w <= 0.0 || css_h <= 0.0 {
            return Err(GeometryError {
                detail: "invalid CSS viewport".to_string(),
            });
        }
        Ok(Self {
            cols,
            rows,
            content_rows,
            cell,
            physical_w,
            physical_h,
            css_w,
            css_h,
            scale,
            status_rows,
        })
    }

    pub fn cols(self) -> u16 {
        self.cols
    }

    pub fn rows(self) -> u16 {
        self.rows
    }

    pub fn content_rows(self) -> u16 {
        self.content_rows
    }

    pub fn status_rows(self) -> u16 {
        self.status_rows
    }

    pub fn cell(self) -> CellPixels {
        self.cell
    }

    pub fn physical_size(self) -> (u32, u32) {
        (self.physical_w, self.physical_h)
    }

    pub fn css_size(self) -> (f64, f64) {
        (self.css_w, self.css_h)
    }

    pub fn scale(self) -> f64 {
        self.scale
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub probe_budget: Duration,
    pub status_rows: u16,
    pub chunk_bytes: usize,
    pub image_id: u32,
    pub max_png_bytes: usize,
    pub max_encoded_bytes: usize,
    pub max_raster_pixels: u32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            probe_budget: DEFAULT_PROBE_BUDGET,
            status_rows: DEFAULT_STATUS_ROWS,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            image_id: DEFAULT_IMAGE_ID,
            max_png_bytes: MAX_PNG_BYTES,
            max_encoded_bytes: MAX_ENCODED_BYTES,
            max_raster_pixels: MAX_RASTER_PIXELS,
        }
    }
}

impl TerminalConfig {
    pub fn validate(&self) -> Result<(), TerminalError> {
        if self.probe_budget.is_zero() {
            return Err(TerminalError::Config(
                "probe budget must be positive".to_string(),
            ));
        }
        if self.status_rows == 0 {
            return Err(TerminalError::Config(
                "status rows must be positive".to_string(),
            ));
        }
        if self.chunk_bytes == 0 || self.chunk_bytes > MAX_CHUNK_BYTES {
            return Err(TerminalError::Config(format!(
                "chunk bytes must be 1..={MAX_CHUNK_BYTES}"
            )));
        }
        if self.image_id == 0 || self.image_id == PROBE_IMAGE_ID {
            return Err(TerminalError::Config(
                "image id must be non-zero and distinct from the probe id".to_string(),
            ));
        }
        if self.max_png_bytes == 0 || self.max_encoded_bytes == 0 {
            return Err(TerminalError::Config(
                "frame limits must be positive".to_string(),
            ));
        }
        if self.max_raster_pixels == 0 {
            return Err(TerminalError::Config(
                "raster pixel limit must be positive".to_string(),
            ));
        }
        Ok(())
    }

    pub fn with_probe_budget(mut self, budget: Duration) -> Self {
        self.probe_budget = budget;
        self
    }

    pub fn with_chunk_bytes(mut self, bytes: usize) -> Self {
        self.chunk_bytes = bytes;
        self
    }

    pub fn with_png_limit(mut self, bytes: usize) -> Self {
        self.max_png_bytes = bytes;
        self
    }
}

// ---------------------------------------------------------------------------
// TTY helpers
// ---------------------------------------------------------------------------

pub fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

/// True when both descriptors are TTYs for the same terminal device.
pub fn same_tty(a: RawFd, b: RawFd) -> bool {
    if !is_tty(a) || !is_tty(b) {
        return false;
    }
    let (Some(sa), Some(sb)) = (fstat(a), fstat(b)) else {
        return false;
    };
    if sa.st_rdev != 0 || sb.st_rdev != 0 {
        sa.st_rdev == sb.st_rdev
    } else {
        sa.st_dev == sb.st_dev && sa.st_ino == sb.st_ino
    }
}

fn fstat(fd: RawFd) -> Option<libc::stat> {
    // SAFETY: st is zeroed and the kernel writes it on success.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) == 0 {
            Some(st)
        } else {
            None
        }
    }
}

pub fn terminal_size(fd: RawFd) -> io::Result<(u16, u16)> {
    // SAFETY: winsize is plain data and TIOCGWINSZ writes it on success.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((ws.ws_col, ws.ws_row))
    }
}

// ---------------------------------------------------------------------------
// Raw mode and descriptor I/O
// ---------------------------------------------------------------------------

fn read_termios(fd: RawFd) -> io::Result<libc::termios> {
    // SAFETY: termios is zeroed and filled by tcgetattr.
    unsafe {
        let mut term: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut term) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(term)
    }
}

fn set_raw(fd: RawFd) -> io::Result<()> {
    let original = read_termios(fd)?;
    let mut raw = original;
    // SAFETY: cfmakeraw only mutates the provided termios value.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: tcsetattr applies the termios value to this terminal.
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn restore_termios(fd: RawFd, saved: &libc::termios) -> io::Result<()> {
    // SAFETY: tcsetattr applies the saved termios value.
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, saved) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_fd(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: the pointer and length are valid for this call.
        let written =
            unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        if written < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short terminal write",
            ));
        }
        offset += written as usize;
    }
    Ok(())
}

fn read_fd(fd: RawFd, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: the buffer pointer and length are valid for this call.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(read as usize);
    }
}

fn poll_readable(fd: RawFd, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = if timeout.is_zero() {
        0
    } else {
        timeout.as_millis().clamp(1, i32::MAX as u128) as i32
    };
    loop {
        // SAFETY: pfd is a single initialized pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Ok(false);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("terminal descriptor error"));
        }
        return Ok(true);
    }
}

// ---------------------------------------------------------------------------
// Reply / key scanner
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellDims {
    width: u16,
    height: u16,
}

impl CellDims {
    fn cell(self) -> Option<CellPixels> {
        CellPixels::new(self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KittyReply {
    Ok,
    Negative(String),
}

#[derive(Debug, Default)]
struct Scan {
    keys: Vec<u8>,
    cell: Option<CellDims>,
    kitty: Option<KittyReply>,
    unexpected: Option<String>,
    incomplete: bool,
    saw_protocol: bool,
}

/// Scan a complete prefix of `bytes`. The returned count is the consumed
/// length; anything left is an incomplete escape sequence that the caller must
/// retain for the next read.
fn scan_bytes(bytes: &[u8]) -> (usize, Scan) {
    let mut scan = Scan::default();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte != 0x1b {
            let start = i;
            while i < bytes.len() && bytes[i] != 0x1b {
                i += 1;
            }
            scan.keys.extend_from_slice(&bytes[start..i]);
            continue;
        }

        // An escape at the end of the buffer is ambiguous: it may be a lone
        // Escape key or the start of a sequence. Retain it.
        if i + 1 >= bytes.len() {
            scan.incomplete = true;
            break;
        }

        match bytes[i + 1] {
            b'[' => {
                let start = i + 2;
                let Some(end) = bytes[start..]
                    .iter()
                    .position(|b| (0x40..=0x7e).contains(b))
                    .map(|offset| start + offset)
                else {
                    scan.incomplete = true;
                    break;
                };
                let params = &bytes[start..end];
                let final_byte = bytes[end];
                if final_byte == b't' && params.first() == Some(&b'6') {
                    scan.saw_protocol = true;
                    match parse_cell_params(params) {
                        Ok(cell) => scan.cell = Some(cell),
                        Err(detail) => scan.unexpected = Some(detail),
                    }
                } else {
                    scan.keys.extend_from_slice(&bytes[i..=end]);
                }
                i = end + 1;
            }
            b'_' => {
                // APC ... ST. Kitty replies are `ESC _ G ... ESC \`.
                let Some(rel_end) = find_st(&bytes[i + 2..]) else {
                    scan.incomplete = true;
                    break;
                };
                let payload = &bytes[i + 2..i + 2 + rel_end];
                let consumed = i + 2 + rel_end + 2;
                if payload.first() != Some(&b'G') {
                    scan.keys.extend_from_slice(&bytes[i..consumed]);
                    i = consumed;
                    continue;
                }
                scan.saw_protocol = true;
                match parse_kitty_payload(payload) {
                    Ok(reply) => scan.kitty = Some(reply),
                    Err(detail) => scan.unexpected = Some(detail),
                }
                i = consumed;
            }
            b']' | b'P' => {
                // OSC or DCS. Preserve complete sequences as ordinary bytes;
                // retain incomplete ones.
                let Some(rel_end) = find_string_terminator(&bytes[i + 2..]) else {
                    scan.incomplete = true;
                    break;
                };
                let consumed = i + 2 + rel_end;
                scan.keys.extend_from_slice(&bytes[i..consumed]);
                i = consumed;
            }
            b'O' => {
                if i + 2 >= bytes.len() {
                    scan.incomplete = true;
                    break;
                }
                scan.keys.extend_from_slice(&bytes[i..i + 3]);
                i += 3;
            }
            _ => {
                // Two-byte escape (Alt+key, save/restore, etc.).
                scan.keys.extend_from_slice(&bytes[i..i + 2]);
                i += 2;
            }
        }
    }
    (i, scan)
}

/// Index of `ESC \` relative to the slice start, if present.
fn find_st(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'\\' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// OSC ends with BEL or ST; DCS ends with ST. Return the consumed length.
fn find_string_terminator(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x07 {
            return Some(i + 1);
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

fn parse_cell_params(params: &[u8]) -> Result<CellDims, String> {
    let parts: Vec<&[u8]> = params.split(|b| *b == b';').collect();
    if parts.len() != 3 || parts[0] != b"6" {
        return Err(format!(
            "malformed cell-size reply: {}",
            String::from_utf8_lossy(params)
        ));
    }
    let height = parse_u16(parts[1]).ok_or_else(|| "malformed cell height".to_string())?;
    let width = parse_u16(parts[2]).ok_or_else(|| "malformed cell width".to_string())?;
    Ok(CellDims { width, height })
}

fn parse_u16(bytes: &[u8]) -> Option<u16> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn parse_kitty_payload(payload: &[u8]) -> Result<KittyReply, String> {
    let body = payload
        .strip_prefix(b"G")
        .ok_or_else(|| "malformed Kitty APC".to_string())?;
    let Some(semi) = body.iter().position(|b| *b == b';') else {
        return Err("Kitty reply missing separator".to_string());
    };
    let control = &body[..semi];
    let message = &body[semi + 1..];
    let mut id = None;
    for key in control.split(|b| *b == b',') {
        if let Some(value) = key.strip_prefix(b"i=") {
            id = parse_u32(value);
        }
    }
    match id {
        Some(id) if id == PROBE_IMAGE_ID => {}
        Some(id) => {
            return Err(format!("Kitty reply for foreign image id {id}"));
        }
        None => return Err("Kitty reply missing image id".to_string()),
    }
    let text = String::from_utf8_lossy(message);
    if text == "OK" {
        Ok(KittyReply::Ok)
    } else if text.starts_with('E') {
        Ok(KittyReply::Negative(text.into_owned()))
    } else {
        Err(format!("unexpected Kitty reply: {text}"))
    }
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeReport {
    pub cell: CellPixels,
    pub elapsed: Duration,
}

fn probe_terminal(
    input_fd: RawFd,
    output_fd: RawFd,
    budget: Duration,
    keys: &mut Vec<u8>,
) -> Result<ProbeReport, ProbeError> {
    let started = Instant::now();
    let deadline = started + budget;
    // `CSI 16 t` asks for the cell size; the Kitty query confirms graphics
    // support without creating an image. Send both, then read one shared
    // budget.
    let queries = format!("\x1b[16t\x1b_Gi={PROBE_IMAGE_ID},a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\");
    if let Err(err) = write_fd(output_fd, queries.as_bytes()) {
        return Err(ProbeError {
            kind: ProbeFailureKind::UnsupportedGraphics,
            detail: format!("cannot write probe: {err}"),
            elapsed: started.elapsed(),
        });
    }

    let mut buffer: Vec<u8> = Vec::new();
    let mut graphics: Option<KittyReply> = None;
    let mut cell: Option<CellDims> = None;
    let mut unexpected: Option<String> = None;
    let mut saw_reply = false;
    #[allow(unused_assignments)]
    let mut incomplete = false;

    loop {
        let (consumed, scan) = scan_bytes(&buffer);
        if consumed > 0 {
            buffer.drain(..consumed);
        }
        incomplete = scan.incomplete;
        saw_reply |= scan.saw_protocol;
        keys.extend_from_slice(&scan.keys);
        if let Some(cell_dims) = scan.cell {
            cell = Some(cell_dims);
        }
        if let Some(reply) = scan.kitty {
            if let KittyReply::Negative(detail) = &reply {
                return Err(ProbeError {
                    kind: ProbeFailureKind::UnsupportedGraphics,
                    detail: detail.clone(),
                    elapsed: started.elapsed(),
                });
            }
            graphics = Some(reply);
        }
        if let Some(detail) = scan.unexpected {
            unexpected = Some(detail);
        }

        if let Some(detail) = unexpected {
            return Err(ProbeError {
                kind: ProbeFailureKind::UnexpectedReply,
                detail,
                elapsed: started.elapsed(),
            });
        }
        if graphics.is_some() && cell.and_then(CellDims::cell).is_some() {
            return Ok(ProbeReport {
                cell: cell.and_then(CellDims::cell).expect("checked"),
                elapsed: started.elapsed(),
            });
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match poll_readable(input_fd, deadline - now) {
            Ok(false) => break,
            Ok(true) => {
                let mut chunk = [0u8; 1024];
                match read_fd(input_fd, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    Err(err) => {
                        return Err(ProbeError {
                            kind: ProbeFailureKind::Timeout,
                            detail: format!("probe read: {err}"),
                            elapsed: started.elapsed(),
                        });
                    }
                }
            }
            Err(err) => {
                return Err(ProbeError {
                    kind: ProbeFailureKind::Timeout,
                    detail: format!("probe poll: {err}"),
                    elapsed: started.elapsed(),
                });
            }
        }
    }

    // A lone Escape at the deadline is a retained ordinary key, not a partial
    // protocol reply.
    if buffer.len() >= 2 && buffer[0] == 0x1b {
        incomplete = true;
    }
    if buffer == [0x1b] {
        keys.push(0x1b);
    }

    let elapsed = started.elapsed();
    if graphics.is_none() && cell.is_none() && !saw_reply {
        return Err(ProbeError {
            kind: if incomplete {
                ProbeFailureKind::IncompleteReply
            } else {
                ProbeFailureKind::Timeout
            },
            detail: if incomplete {
                "partial probe reply at deadline".to_string()
            } else {
                "no probe replies before deadline".to_string()
            },
            elapsed,
        });
    }

    match (graphics, cell) {
        (Some(KittyReply::Ok), Some(dims)) => match dims.cell() {
            Some(cell) => Ok(ProbeReport { cell, elapsed }),
            None => Err(ProbeError {
                kind: ProbeFailureKind::MissingGeometry,
                detail: format!("invalid cell size {}x{}", dims.width, dims.height),
                elapsed,
            }),
        },
        (Some(KittyReply::Ok), None) => Err(ProbeError {
            kind: if incomplete {
                ProbeFailureKind::IncompleteReply
            } else {
                ProbeFailureKind::MissingGeometry
            },
            detail: if incomplete {
                "partial cell-size reply at deadline".to_string()
            } else {
                "graphics supported but no cell size reply".to_string()
            },
            elapsed,
        }),
        (None, Some(_)) => Err(ProbeError {
            kind: ProbeFailureKind::UnsupportedGraphics,
            detail: "cell size answered but graphics query did not".to_string(),
            elapsed,
        }),
        (None, None) => Err(ProbeError {
            kind: if incomplete {
                ProbeFailureKind::IncompleteReply
            } else {
                ProbeFailureKind::Timeout
            },
            detail: "no complete probe replies".to_string(),
            elapsed,
        }),
        (Some(KittyReply::Negative(detail)), _) => Err(ProbeError {
            kind: ProbeFailureKind::UnsupportedGraphics,
            detail,
            elapsed,
        }),
    }
}

// ---------------------------------------------------------------------------
// Input batch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBatch {
    /// Ordinary key bytes, exactly as read, with interrupt bytes removed.
    pub bytes: Vec<u8>,
    /// A raw `0x03` was read (raw mode turns Ctrl+C into this byte).
    pub interrupt: bool,
}

impl InputBatch {
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.interrupt
    }
}
// ---------------------------------------------------------------------------
// Frame reporting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub png_bytes: usize,
    pub encoded_bytes: usize,
    pub chunks: usize,
    pub wire_bytes: usize,
}

// ---------------------------------------------------------------------------
// Terminal owner
// ---------------------------------------------------------------------------

pub struct TerminalOwner {
    input_fd: RawFd,
    output_fd: RawFd,
    _input_file: Option<File>,
    _output_file: Option<File>,
    saved_termios: Option<libc::termios>,
    config: TerminalConfig,
    geometry: Geometry,
    environment: EnvironmentHint,
    probe: ProbeReport,
    /// Parsed ordinary keys not yet returned to the caller.
    keys_pending: Vec<u8>,
    /// Incomplete escape sequence retained between reads.
    input_buffer: Vec<u8>,
    image_owned: bool,
    alternate_active: bool,
    cursor_hidden: bool,
    finished: bool,
}

impl fmt::Debug for TerminalOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalOwner")
            .field("geometry", &self.geometry)
            .field("image_id", &self.config.image_id)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl TerminalOwner {
    /// Open the standard TTY for output and the controlling terminal for input.
    ///
    /// `stdout` must be a TTY. If `stdin` is not the same TTY (for example
    /// `cat report.html | glisk -`), `/dev/tty` is opened for input and checked
    /// against the output device. A missing or mismatched TTY is an error, so
    /// the caller can choose plain text.
    #[allow(dead_code)] // Wired in by bead glisk-2hp.8.
    pub fn open() -> Result<Self, TerminalError> {
        Self::open_with_config(TerminalConfig::default())
    }

    #[allow(dead_code)] // Wired in by bead glisk-2hp.8.
    pub fn open_with_config(config: TerminalConfig) -> Result<Self, TerminalError> {
        config.validate()?;
        let output_fd = libc::STDOUT_FILENO;
        if !is_tty(output_fd) {
            return Err(TerminalError::NotATerminal("stdout"));
        }
        let stdin_fd = libc::STDIN_FILENO;
        let (input_fd, input_file) = if is_tty(stdin_fd) && same_tty(stdin_fd, output_fd) {
            (stdin_fd, None)
        } else {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|err| TerminalError::TtyOpen(err.to_string()))?;
            let fd = file.as_raw_fd();
            if !same_tty(fd, output_fd) {
                return Err(TerminalError::NotSameTerminal);
            }
            (fd, Some(file))
        };
        Self::establish(input_fd, output_fd, input_file, None, config)
    }

    /// Test/integration constructor. The descriptors must both be TTYs for the
    /// same terminal; neither is closed by the owner.
    pub fn from_fds(
        input_fd: RawFd,
        output_fd: RawFd,
        config: TerminalConfig,
    ) -> Result<Self, TerminalError> {
        Self::establish(input_fd, output_fd, None, None, config)
    }

    fn establish(
        input_fd: RawFd,
        output_fd: RawFd,
        input_file: Option<File>,
        output_file: Option<File>,
        config: TerminalConfig,
    ) -> Result<Self, TerminalError> {
        config.validate()?;
        if !is_tty(input_fd) {
            return Err(TerminalError::NotATerminal("input"));
        }
        if !is_tty(output_fd) {
            return Err(TerminalError::NotATerminal("output"));
        }
        if !same_tty(input_fd, output_fd) {
            return Err(TerminalError::NotSameTerminal);
        }

        let saved =
            read_termios(input_fd).map_err(|err| TerminalError::RawMode(err.to_string()))?;
        set_raw(input_fd).map_err(|err| TerminalError::RawMode(err.to_string()))?;

        let mut pending_keys = Vec::new();
        let probe =
            match probe_terminal(input_fd, output_fd, config.probe_budget, &mut pending_keys) {
                Ok(report) => report,
                Err(err) => {
                    let _ = restore_termios(input_fd, &saved);
                    return Err(TerminalError::Probe(err));
                }
            };

        let (cols, rows) = match terminal_size(output_fd) {
            Ok(size) => size,
            Err(err) => {
                let _ = restore_termios(input_fd, &saved);
                return Err(TerminalError::Size(err.to_string()));
            }
        };
        let geometry = match Geometry::compute_with_cap(
            cols,
            rows,
            probe.cell,
            config.status_rows,
            config.max_raster_pixels,
        ) {
            Ok(geometry) => geometry,
            Err(err) => {
                let _ = restore_termios(input_fd, &saved);
                return Err(TerminalError::Geometry(err));
            }
        };

        if let Err(err) =
            write_fd(output_fd, ENTER_ALT).and_then(|_| write_fd(output_fd, HIDE_CURSOR))
        {
            let _ = restore_termios(input_fd, &saved);
            return Err(TerminalError::Io {
                op: "enter alternate screen",
                source: err,
            });
        }

        Ok(Self {
            input_fd,
            output_fd,
            _input_file: input_file,
            _output_file: output_file,
            saved_termios: Some(saved),
            config,
            geometry,
            environment: EnvironmentHint::detect(),
            probe,
            keys_pending: pending_keys,
            input_buffer: Vec::new(),
            image_owned: false,
            alternate_active: true,
            cursor_hidden: true,
            finished: false,
        })
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn image_id(&self) -> u32 {
        self.config.image_id
    }

    pub fn environment(&self) -> &EnvironmentHint {
        &self.environment
    }

    pub fn probe_report(&self) -> ProbeReport {
        self.probe
    }

    /// Read the current grid size via `TIOCGWINSZ` and recompute geometry when
    /// it changed. Cell pixel size is retained; call [`Self::resize_with_cell`]
    /// after a font-size change, or [`Self::reprobe_cell`] to measure it again.
    pub fn refresh_size(&mut self) -> Result<Option<Geometry>, TerminalError> {
        self.ensure_live()?;
        let (cols, rows) =
            terminal_size(self.output_fd).map_err(|err| TerminalError::Size(err.to_string()))?;
        if cols == self.geometry.cols() && rows == self.geometry.rows() {
            return Ok(None);
        }
        Ok(Some(self.resize(cols, rows)?))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<Geometry, TerminalError> {
        self.resize_with_cell(cols, rows, self.geometry.cell())
    }

    pub fn resize_with_cell(
        &mut self,
        cols: u16,
        rows: u16,
        cell: CellPixels,
    ) -> Result<Geometry, TerminalError> {
        self.ensure_live()?;
        let geometry = Geometry::compute_with_cap(
            cols,
            rows,
            cell,
            self.config.status_rows,
            self.config.max_raster_pixels,
        )?;
        self.geometry = geometry;
        Ok(geometry)
    }

    /// Re-run only the bounded cell-size query, retaining ordinary keys. Used
    /// after a font-size change, not on every resize event.
    pub fn reprobe_cell(&mut self) -> Result<CellPixels, TerminalError> {
        self.ensure_live()?;
        let started = Instant::now();
        let deadline = started + self.config.probe_budget;
        if let Err(err) = write_fd(self.output_fd, b"\x1b[16t") {
            return Err(TerminalError::Io {
                op: "cell reprobe write",
                source: err,
            });
        }
        let mut buffer = std::mem::take(&mut self.input_buffer);
        loop {
            let (consumed, scan) = scan_bytes(&buffer);
            if consumed > 0 {
                buffer.drain(..consumed);
            }
            self.keys_pending.extend_from_slice(&scan.keys);
            if let Some(cell) = scan.cell.and_then(CellDims::cell) {
                self.input_buffer = buffer;
                return Ok(cell);
            }
            if let Some(detail) = scan.unexpected {
                self.input_buffer = buffer;
                return Err(TerminalError::Probe(ProbeError {
                    kind: ProbeFailureKind::UnexpectedReply,
                    detail,
                    elapsed: started.elapsed(),
                }));
            }
            let now = Instant::now();
            if now >= deadline {
                self.input_buffer = buffer;
                return Err(TerminalError::Probe(ProbeError {
                    kind: if scan.incomplete {
                        ProbeFailureKind::IncompleteReply
                    } else {
                        ProbeFailureKind::Timeout
                    },
                    detail: "no cell-size reply".to_string(),
                    elapsed: started.elapsed(),
                }));
            }
            match poll_readable(self.input_fd, deadline - now) {
                Ok(false) => {}
                Ok(true) => {
                    let mut chunk = [0u8; 512];
                    match read_fd(self.input_fd, &mut chunk) {
                        Ok(0) => {
                            self.input_buffer = buffer;
                            return Err(TerminalError::InputClosed);
                        }
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        Err(err) => {
                            self.input_buffer = buffer;
                            return Err(TerminalError::Io {
                                op: "cell reprobe read",
                                source: err,
                            });
                        }
                    }
                }
                Err(err) => {
                    self.input_buffer = buffer;
                    return Err(TerminalError::Io {
                        op: "cell reprobe poll",
                        source: err,
                    });
                }
            }
        }
    }

    /// Poll for key input. Probe replies that arrive late are consumed; ordinary
    /// bytes are returned unchanged. A `0x03` byte sets [`InputBatch::interrupt`]
    /// and is removed from `bytes`; the caller must then quit and call
    /// [`Self::finish`] (or drop the owner).
    pub fn poll_input(&mut self, timeout: Duration) -> Result<Option<InputBatch>, TerminalError> {
        self.ensure_live()?;
        if !self.keys_pending.is_empty() {
            return Ok(Some(self.take_batch()));
        }
        let ready = poll_readable(self.input_fd, timeout).map_err(|err| TerminalError::Io {
            op: "input poll",
            source: err,
        })?;
        if !ready {
            // A lone Escape is ambiguous; after an idle poll treat it as a key
            // so the pager can dismiss search or fall back.
            if self.input_buffer == [0x1b] {
                self.input_buffer.clear();
                self.keys_pending.push(0x1b);
                return Ok(Some(self.take_batch()));
            }
            return Ok(None);
        }
        let mut chunk = [0u8; 4096];
        let read = read_fd(self.input_fd, &mut chunk).map_err(|err| TerminalError::Io {
            op: "input read",
            source: err,
        })?;
        if read == 0 {
            return Err(TerminalError::InputClosed);
        }
        self.input_buffer.extend_from_slice(&chunk[..read]);
        let (consumed, scan) = scan_bytes(&self.input_buffer);
        if consumed > 0 {
            self.input_buffer.drain(..consumed);
        }
        // Late probe replies and foreign protocol replies are consumed, not
        // delivered as keys.
        self.keys_pending.extend_from_slice(&scan.keys);
        if self.keys_pending.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.take_batch()))
    }

    fn take_batch(&mut self) -> InputBatch {
        let mut batch = InputBatch::default();
        for byte in std::mem::take(&mut self.keys_pending) {
            if byte == 0x03 {
                batch.interrupt = true;
            } else {
                batch.bytes.push(byte);
            }
        }
        batch
    }

    /// Transmit one viewport PNG and place it above the reserved status row with
    /// the stable image id. The cursor is saved/restored around the write, and
    /// `C=1` keeps placement from moving it. Every call replaces the previous
    /// image of the same id.
    pub fn present(&mut self, png: &[u8]) -> Result<FrameStats, TerminalError> {
        self.ensure_live()?;
        if !png.starts_with(PNG_SIGNATURE) {
            return Err(TerminalError::InvalidPng);
        }
        if png.len() > self.config.max_png_bytes {
            return Err(TerminalError::FrameTooLarge {
                png_bytes: png.len(),
                encoded_bytes: 0,
                limit: self.config.max_png_bytes,
            });
        }
        let encoded = BASE64.encode(png);
        if encoded.len() > self.config.max_encoded_bytes {
            return Err(TerminalError::FrameTooLarge {
                png_bytes: png.len(),
                encoded_bytes: encoded.len(),
                limit: self.config.max_encoded_bytes,
            });
        }

        let geometry = self.geometry;
        let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(self.config.chunk_bytes).collect();
        let mut wire = Vec::with_capacity(encoded.len() + chunks.len() * 24 + 16);
        wire.extend_from_slice(SAVE_CURSOR);
        wire.extend_from_slice(b"\x1b[H");
        for (index, chunk) in chunks.iter().enumerate() {
            let more = usize::from(index + 1 < chunks.len());
            if index == 0 {
                wire.extend_from_slice(
                    format!(
                        "\x1b_Ga=T,f=100,i={},q=2,C=1,c={},r={},m={more};",
                        self.config.image_id,
                        geometry.cols(),
                        geometry.content_rows()
                    )
                    .as_bytes(),
                );
            } else {
                wire.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
            }
            wire.extend_from_slice(chunk);
            wire.extend_from_slice(b"\x1b\\");
        }
        wire.extend_from_slice(RESTORE_CURSOR);

        // Mark the image as owned before the write: a partial transmission can
        // still register the image, and cleanup must delete it.
        self.image_owned = true;
        write_fd(self.output_fd, &wire).map_err(|err| TerminalError::Io {
            op: "viewport frame write",
            source: err,
        })?;
        Ok(FrameStats {
            png_bytes: png.len(),
            encoded_bytes: encoded.len(),
            chunks: chunks.len(),
            wire_bytes: wire.len(),
        })
    }

    /// Paint the last terminal row. Control characters are stripped and the text
    /// is truncated to the grid width. The cursor is preserved.
    pub fn paint_status(&mut self, text: &str) -> Result<(), TerminalError> {
        self.ensure_live()?;
        let clean = sanitize_status(text, self.geometry.cols());
        let sequence = format!("\x1b7\x1b[{};1H\x1b[2K{clean}\x1b8", self.geometry.rows());
        write_fd(self.output_fd, sequence.as_bytes()).map_err(|err| TerminalError::Io {
            op: "status row write",
            source: err,
        })
    }

    /// Explicit cleanup: delete the owned image, show the cursor, leave the
    /// alternate screen, and restore saved termios. Idempotent and called from
    /// `Drop` as a backstop.
    pub fn finish(&mut self) -> Result<(), TerminalError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        let mut sequence = Vec::new();
        if self.image_owned {
            sequence.extend_from_slice(
                format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.config.image_id).as_bytes(),
            );
            self.image_owned = false;
        }
        if self.cursor_hidden {
            sequence.extend_from_slice(SHOW_CURSOR);
            self.cursor_hidden = false;
        }
        if self.alternate_active {
            sequence.extend_from_slice(LEAVE_ALT);
            self.alternate_active = false;
        }

        let write_result = if sequence.is_empty() {
            Ok(())
        } else {
            write_fd(self.output_fd, &sequence)
        };
        let restore_result = match self.saved_termios.take() {
            Some(saved) => restore_termios(self.input_fd, &saved),
            None => Ok(()),
        };

        if let Err(err) = write_result {
            return Err(TerminalError::Io {
                op: "terminal cleanup write",
                source: err,
            });
        }
        if let Err(err) = restore_result {
            return Err(TerminalError::RawMode(format!(
                "restore terminal modes: {err}"
            )));
        }
        Ok(())
    }

    fn ensure_live(&self) -> Result<(), TerminalError> {
        if self.finished {
            Err(TerminalError::Finished)
        } else {
            Ok(())
        }
    }
}

impl Drop for TerminalOwner {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn sanitize_status(text: &str, cols: u16) -> String {
    let mut chars = text.chars().peekable();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => output.push(' '),
            '\x1b' => match chars.peek().copied() {
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
            },
            control if control.is_control() => {}
            visible => output.push(visible),
        }
    }
    let max = usize::from(cols);
    if output.chars().count() > max {
        output = output.chars().take(max.saturating_sub(1)).collect();
        output.push('…');
    }
    output
}

// ---------------------------------------------------------------------------
// Tests for parser details that do not need a pseudo-terminal.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn scanner_retains_probe_replies_and_keys() {
        let bytes = b"\x1b[6;34;17t\x1b_Gi=31;OK\x1b\\j";
        let (consumed, scan) = scan_bytes(bytes);
        assert_eq!(consumed, bytes.len());
        assert_eq!(
            scan.cell,
            Some(CellDims {
                width: 17,
                height: 34
            })
        );
        assert_eq!(scan.kitty, Some(KittyReply::Ok));
        assert_eq!(scan.keys, b"j");
    }

    #[test]
    fn scanner_marks_partial_reply_incomplete() {
        let bytes = b"\x1b_Gi=31;OK\x1b\\\x1b[6;34";
        let (consumed, scan) = scan_bytes(bytes);
        assert_eq!(consumed, 12);
        assert!(scan.incomplete);
        assert_eq!(scan.kitty, Some(KittyReply::Ok));
    }

    #[test]
    fn status_is_sanitized_and_truncated() {
        assert_eq!(sanitize_status("hi\x1b[31mthere\n", 8), "hithere");
        assert_eq!(sanitize_status("abcdef", 4), "abc…");
    }

    #[test]
    fn geometry_validates_dimensions() {
        let cell = CellPixels::new(16, 32).unwrap();
        let geometry = Geometry::compute(80, 24, cell, 1).unwrap();
        assert_eq!(geometry.content_rows(), 23);
        assert_eq!(geometry.physical_size(), (1280, 736));
        assert_eq!(geometry.css_size(), (640.0, 368.0));
        assert_eq!(geometry.scale(), 2.0);
        assert!(Geometry::compute(80, 1, cell, 1).is_err());
        assert!(Geometry::compute(1, 24, cell, 1).is_err());
        assert!(Geometry::compute(80, 24, cell, 0).is_err());
    }
}
