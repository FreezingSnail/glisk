//! tmux pane support for the Glisk live pager (bead `glisk-2hp.11`).
//!
//! The direct Kitty path in [`crate::terminal`] cannot see the outer terminal
//! through tmux: the pane PTY answers neither the cell-size query nor the
//! graphics query, and tmux clips ordinary Kitty placements to nothing. This
//! module owns the supported tmux path:
//!
//! - detect the pane from `TMUX`, `TMUX_PANE`, and the stdout TTY;
//! - ask the pane's tmux server, read-only and inside a fixed budget, for the
//!   tmux version, pane identity/visibility, effective pane
//!   `allow-passthrough`, the attached client, its terminal name/type, and its
//!   measured cell pixels;
//! - wrap every Kitty graphics command in tmux's `ESC Ptmux;...ESC \`
//!   passthrough form, doubling embedded ESC bytes;
//! - build the Kitty Unicode placeholder grid (`U+10EEEE` plus row/column
//!   combining diacritics and the image id in the truecolor foreground) as
//!   ordinary pane output, so tmux owns layout, clipping, and redraw;
//! - bound every query, image, grid, and protocol write;
//! - re-check visibility/geometry on a bounded interval.
//!
//! Only one tmux layer with one attached Ghostty 1.4+ display client is in the
//! initial supported matrix. Anything ambiguous returns an actionable
//! [`TmuxError`]; the caller then writes plain text.

use std::{
    env, fmt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Budget for each read-only tmux query. Fixed arguments, bounded output.
pub const QUERY_BUDGET: Duration = Duration::from_millis(250);
/// Minimum interval between visibility/geometry re-checks in the event loop.
pub const SYNC_INTERVAL: Duration = Duration::from_millis(500);
/// The pane-local `allow-passthrough` option is available from tmux 3.5; 3.5a is the verified baseline.
pub const MIN_TMUX: (u32, u32) = (3, 5);
/// Verified Ghostty target: Unicode placeholder conformance in 1.4.
pub const MIN_GHOSTTY: (u32, u32) = (1, 4);
/// Bound on any single tmux query response.
pub const MAX_QUERY_BYTES: usize = 8192;
/// Bound on the emitted placeholder grid. The diacritic table has 297 entries.
pub const MAX_GRID_OUTPUT_BYTES: usize = 1024 * 1024;
/// Image ids carried by a 24-bit truecolor foreground are limited to 24 bits.
pub const MAX_IMAGE_ID: u32 = 0x00ff_ffff;
const PROBE_IMAGE_ID: u32 = 31;
const FIELD_SEP: char = '|';

/// Kitty's row/column combining diacritics (297 values, index = cell row or
/// column). Source: kitty `rowcolumn-diacritics.txt`.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// Stable failure categories for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxFailureKind {
    /// `TMUX`/`TMUX_PANE` are absent, malformed, or unusable.
    Environment,
    /// The read-only tmux query failed, timed out, or returned junk.
    Query,
    /// The pane's TTY is not the stdout TTY.
    PaneMismatch,
    /// The tmux server is older than 3.5.
    OldTmux,
    /// The effective pane `allow-passthrough` is off.
    PassthroughOff,
    /// The session does not have exactly one attached display client.
    ClientCount,
    /// The attached client is not a supported Ghostty client.
    UnsupportedClient,
    /// The attached Ghostty client is older than 1.4.
    OldGhostty,
    /// tmux would quantize the 24-bit image-id color.
    NoTruecolor,
    /// The pane would not isolate/restore Glisk's alternate screen.
    AlternateScreenOff,
    /// Cell pixels or pane geometry are missing or implausible.
    Geometry,
    /// The grid needs more rows/columns than the diacritic table encodes.
    GridTooLarge,
    /// The image id is outside the 24-bit placeholder range.
    ImageId,
}

impl TmuxFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Query => "query",
            Self::PaneMismatch => "pane-mismatch",
            Self::OldTmux => "old-tmux",
            Self::PassthroughOff => "passthrough-off",
            Self::ClientCount => "client-count",
            Self::UnsupportedClient => "unsupported-client",
            Self::OldGhostty => "old-ghostty",
            Self::NoTruecolor => "no-truecolor",
            Self::AlternateScreenOff => "alternate-screen-off",
            Self::Geometry => "geometry",
            Self::GridTooLarge => "grid-too-large",
            Self::ImageId => "image-id",
        }
    }
}

/// Actionable tmux detection failure. The display text is what the automatic
/// fallback prints on stderr before writing plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxError {
    pub kind: TmuxFailureKind,
    pub detail: String,
}

impl TmuxError {
    fn new(kind: TmuxFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for TmuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tmux support {}: {}", self.kind.as_str(), self.detail)
    }
}

impl std::error::Error for TmuxError {}

/// Effective pane-level `allow-passthrough`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Passthrough {
    Off,
    /// Allowed only while the pane is visible.
    Visible,
    /// Allowed even while the pane is hidden.
    All,
}

impl Passthrough {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().trim_end_matches('*') {
            "off" => Some(Self::Off),
            "on" => Some(Self::Visible),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Visible => "on",
            Self::All => "all",
        }
    }

    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// One validated tmux pane plus the attached Ghostty client that owns it.
#[derive(Debug, Clone)]
pub struct TmuxPane {
    socket: String,
    pane_id: String,
    session_id: String,
    pane_tty: String,
    client_tty: String,
    client_termname: String,
    client_termtype: String,
    client_pid: u32,
    tmux_version: String,
    passthrough: Passthrough,
    alternate_screen: bool,
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
    attached_clients: usize,
    visible: bool,
    pane_active: bool,
    window_active: bool,
    window_zoomed: bool,
    pane_dead: bool,
}

impl TmuxPane {
    pub fn socket(&self) -> &str {
        &self.socket
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pane_tty(&self) -> &str {
        &self.pane_tty
    }

    pub fn client_tty(&self) -> &str {
        &self.client_tty
    }

    pub fn client_termname(&self) -> &str {
        &self.client_termname
    }

    pub fn client_termtype(&self) -> &str {
        &self.client_termtype
    }

    pub fn client_pid(&self) -> u32 {
        self.client_pid
    }

    pub fn tmux_version(&self) -> &str {
        &self.tmux_version
    }

    pub fn passthrough(&self) -> Passthrough {
        self.passthrough
    }

    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cell_width(&self) -> u16 {
        self.cell_width
    }

    pub fn cell_height(&self) -> u16 {
        self.cell_height
    }

    pub fn attached_clients(&self) -> usize {
        self.attached_clients
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn summary(&self) -> String {
        format!(
            "tmux {} pane {} client {} ({}) passthrough={} alternate-screen={}",
            self.tmux_version,
            self.pane_id,
            self.client_termname,
            self.client_termtype,
            self.passthrough.as_str(),
            if self.alternate_screen { "on" } else { "off" }
        )
    }

    /// Re-read the pane and attached client. Uses the stored socket/pane target
    /// and the same fixed, read-only queries as detection.
    ///
    /// A detached session (zero clients) is reported as hidden and keeps the
    /// last measured client geometry; a second attached client or a changed,
    /// unsupported client is an error. The caller re-presents its latest frame
    /// when a pane becomes visible again.
    pub fn sync(&mut self) -> Result<TmuxRefresh, TmuxError> {
        let before_visible = self.visible;
        let before = (self.cols, self.rows, self.cell_width, self.cell_height);
        let info = query_pane(&self.socket, &self.pane_id, QUERY_BUDGET)?;
        apply_pane_info(self, info)?;

        let clients = query_clients(&self.socket, &self.session_id, QUERY_BUDGET)?;
        if clients.len() > 1 {
            return Err(TmuxError::new(
                TmuxFailureKind::ClientCount,
                format!(
                    "session {} has {} attached clients; attach exactly one Ghostty 1.4+ client",
                    self.session_id,
                    clients.len()
                ),
            ));
        }
        if let Some(client) = clients.first() {
            apply_client(self, client)?;
        } else {
            self.attached_clients = 0;
            self.visible = false;
        }

        let passthrough = query_passthrough(&self.socket, &self.pane_id, QUERY_BUDGET)?;
        if !passthrough.enabled() {
            return Err(TmuxError::new(
                TmuxFailureKind::PassthroughOff,
                format!(
                    "graphics passthrough was disabled for pane {}; restore it with \
                     `tmux set-option -p allow-passthrough on`",
                    self.pane_id
                ),
            ));
        }
        self.passthrough = passthrough;

        let after = (self.cols, self.rows, self.cell_width, self.cell_height);
        Ok(TmuxRefresh {
            geometry_changed: before != after,
            became_visible: !before_visible && self.visible,
        })
    }
}

/// Result of a bounded re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmuxRefresh {
    pub geometry_changed: bool,
    pub became_visible: bool,
}

/// True when the process environment claims to be inside a tmux pane. The
/// caller must still validate the pane; this only decides whether to attempt
/// the tmux path before the direct Kitty probe.
pub fn is_candidate() -> bool {
    env::var_os("TMUX").is_some() && env::var_os("TMUX_PANE").is_some()
}

/// Detect and validate the pane that owns stdout.
pub fn detect() -> Result<TmuxPane, TmuxError> {
    detect_with_budget(QUERY_BUDGET)
}

fn detect_with_budget(budget: Duration) -> Result<TmuxPane, TmuxError> {
    let tmux = env::var("TMUX").map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Environment,
            "TMUX is not set; run Glisk inside tmux to use the tmux path",
        )
    })?;
    let pane_id = env::var("TMUX_PANE").map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Environment,
            "TMUX_PANE is not set; cannot identify the current pane",
        )
    })?;
    let (socket, _server_pid, _session_index) = parse_tmux_env(&tmux)?;
    validate_pane_id(&pane_id)?;
    if !Path::new(&socket).exists() {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!("tmux socket {socket:?} does not exist"),
        ));
    }
    let stdout_tty = stdout_tty_name().ok_or_else(|| {
        TmuxError::new(
            TmuxFailureKind::PaneMismatch,
            "stdout is not a named terminal; the tmux path needs the pane PTY",
        )
    })?;

    let info = query_pane(&socket, &pane_id, budget)?;
    if info.pane_tty != stdout_tty {
        return Err(TmuxError::new(
            TmuxFailureKind::PaneMismatch,
            format!(
                "pane {} owns {}, but stdout is {}; run Glisk directly in the pane",
                info.pane_id, info.pane_tty, stdout_tty
            ),
        ));
    }
    if info.pane_dead {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!("pane {} is dead", info.pane_id),
        ));
    }
    let (major, minor) = parse_version(&info.version).ok_or_else(|| {
        TmuxError::new(
            TmuxFailureKind::Query,
            format!("cannot parse tmux version {:?}", info.version),
        )
    })?;
    if (major, minor) < MIN_TMUX {
        return Err(TmuxError::new(
            TmuxFailureKind::OldTmux,
            format!(
                "tmux {} is too old; allow-passthrough needs tmux {}.{} or newer",
                info.version, MIN_TMUX.0, MIN_TMUX.1
            ),
        ));
    }

    let passthrough = query_passthrough(&socket, &pane_id, budget)?;
    if !passthrough.enabled() {
        return Err(TmuxError::new(
            TmuxFailureKind::PassthroughOff,
            format!(
                "graphics passthrough is off for pane {}; enable it in that pane with \
                 `tmux set-option -p allow-passthrough on`",
                info.pane_id
            ),
        ));
    }
    let alternate_screen = query_alternate_screen(&socket, &pane_id, budget)?;
    if !alternate_screen {
        return Err(TmuxError::new(
            TmuxFailureKind::AlternateScreenOff,
            format!(
                "pane {} has alternate-screen off, so Glisk cannot restore the original pane \
                 screen; enable it with `tmux set-option -p alternate-screen on`",
                info.pane_id
            ),
        ));
    }

    let clients = query_clients(&socket, &info.session_id, budget)?;
    if clients.len() != 1 {
        return Err(TmuxError::new(
            TmuxFailureKind::ClientCount,
            format!(
                "session {} has {} attached clients; the initial tmux path supports exactly one \
                 attached Ghostty 1.4+ display client",
                info.session_id,
                clients.len()
            ),
        ));
    }
    let client = &clients[0];
    validate_client(client)?;
    let truecolor = query_truecolor(&socket, &client.termname, budget)?;
    if !truecolor {
        return Err(TmuxError::new(
            TmuxFailureKind::NoTruecolor,
            format!(
                "tmux would quantize the 24-bit placeholder image id for client {}; keep \
                 truecolor enabled (Ghostty terminfo or `set -as terminal-overrides \
                 ',{}:Tc'`)",
                client.termname, client.termname
            ),
        ));
    }

    let mut pane = TmuxPane {
        socket,
        pane_id: info.pane_id,
        session_id: info.session_id,
        pane_tty: info.pane_tty,
        client_tty: client.tty.clone(),
        client_termname: client.termname.clone(),
        client_termtype: client.termtype.clone(),
        client_pid: client.pid,
        tmux_version: info.version,
        passthrough,
        alternate_screen,
        cols: info.cols,
        rows: info.rows,
        cell_width: client.cell_width,
        cell_height: client.cell_height,
        attached_clients: 1,
        visible: false,
        pane_active: info.pane_active,
        window_active: info.window_active,
        window_zoomed: info.window_zoomed,
        pane_dead: info.pane_dead,
    };
    pane.visible = visible(
        pane.attached_clients,
        pane.window_active,
        pane.window_zoomed,
        pane.pane_active,
        pane.pane_dead,
    );
    validate_geometry(&pane)?;
    Ok(pane)
}

fn visible(
    attached: usize,
    window_active: bool,
    window_zoomed: bool,
    pane_active: bool,
    pane_dead: bool,
) -> bool {
    attached > 0 && window_active && !pane_dead && (!window_zoomed || pane_active)
}

fn validate_client(client: &ClientInfo) -> Result<(), TmuxError> {
    if client.control_mode != 0 {
        return Err(TmuxError::new(
            TmuxFailureKind::UnsupportedClient,
            "the attached client is in tmux control mode; attach a normal Ghostty client",
        ));
    }
    if !client.utf8 {
        return Err(TmuxError::new(
            TmuxFailureKind::UnsupportedClient,
            format!(
                "client {} is not using UTF-8; Unicode placeholders need a UTF-8 client",
                client.tty
            ),
        ));
    }
    if client.termname != "xterm-ghostty" {
        return Err(TmuxError::new(
            TmuxFailureKind::UnsupportedClient,
            format!(
                "client {} is {:?}; the verified tmux path needs Ghostty 1.4+ \
                 (TERM=xterm-ghostty)",
                client.tty, client.termname
            ),
        ));
    }
    match parse_ghostty_version(&client.termtype) {
        Some((major, minor)) if (major, minor) >= MIN_GHOSTTY => Ok(()),
        Some((major, minor)) => Err(TmuxError::new(
            TmuxFailureKind::OldGhostty,
            format!(
                "Ghostty {major}.{minor} is too old; Unicode placeholder support is verified \
                 on Ghostty {}.{}+",
                MIN_GHOSTTY.0, MIN_GHOSTTY.1
            ),
        )),
        None if client.termtype.trim().is_empty() => {
            // Some tmux servers cannot resolve Ghostty's terminfo and leave
            // client_termtype empty. TERM=xterm-ghostty is still the verified
            // setup; the live evidence records the resolved 1.4.0 termtype.
            Ok(())
        }
        None => Err(TmuxError::new(
            TmuxFailureKind::UnsupportedClient,
            format!(
                "cannot determine the Ghostty version from client type {:?}; Ghostty {}.{}+ is \
                 required",
                client.termtype, MIN_GHOSTTY.0, MIN_GHOSTTY.1
            ),
        )),
    }
}

fn validate_geometry(pane: &TmuxPane) -> Result<(), TmuxError> {
    if pane.cols == 0 || pane.rows == 0 {
        return Err(TmuxError::new(
            TmuxFailureKind::Geometry,
            format!("pane {} reports {}x{}", pane.pane_id, pane.cols, pane.rows),
        ));
    }
    if pane.cell_width == 0 || pane.cell_height == 0 {
        return Err(TmuxError::new(
            TmuxFailureKind::Geometry,
            format!(
                "client {} reports cell size {}x{}; the terminal has not supplied pixel \
                 dimensions",
                pane.client_tty, pane.cell_width, pane.cell_height
            ),
        ));
    }
    Ok(())
}

fn parse_tmux_env(value: &str) -> Result<(String, u32, u32), TmuxError> {
    let mut parts = value.rsplitn(3, ',');
    let session = parts.next().unwrap_or_default();
    let pid = parts.next().unwrap_or_default();
    let socket = parts.next().unwrap_or_default();
    if socket.is_empty() || !socket.starts_with('/') {
        return Err(TmuxError::new(
            TmuxFailureKind::Environment,
            format!("TMUX socket path is malformed: {value:?}"),
        ));
    }
    let pid = pid.parse::<u32>().map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Environment,
            format!("TMUX server pid is malformed: {value:?}"),
        )
    })?;
    let session = session.parse::<u32>().map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Environment,
            format!("TMUX session index is malformed: {value:?}"),
        )
    })?;
    Ok((socket.to_string(), pid, session))
}

fn validate_pane_id(pane: &str) -> Result<(), TmuxError> {
    let digits = pane.strip_prefix('%').unwrap_or_default();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TmuxError::new(
            TmuxFailureKind::Environment,
            format!("TMUX_PANE is not a pane id: {pane:?}"),
        ));
    }
    Ok(())
}

fn stdout_tty_name() -> Option<String> {
    use std::os::fd::AsRawFd;
    let fd = std::io::stdout().as_raw_fd();
    let mut buffer = [0i8; 256];
    // SAFETY: the buffer is writable for its full length; ttyname_r writes a
    // NUL-terminated device name on success.
    let rc = unsafe { libc::ttyname_r(fd, buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: ttyname_r succeeded and NUL-terminated the buffer.
    let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Read-only queries
// ---------------------------------------------------------------------------

const PANE_FORMAT: &str = "#{version}|#{pane_id}|#{pane_tty}|#{pane_width}|#{pane_height}|#{pane_active}|#{window_active}|#{pane_dead}|#{window_zoomed_flag}|#{session_id}|#{session_attached}";
const CLIENT_FORMAT: &str = "#{client_tty}|#{client_termname}|#{client_termtype}|#{client_cell_width}|#{client_cell_height}|#{client_control_mode}|#{client_utf8}|#{client_pid}";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneInfo {
    version: String,
    pane_id: String,
    pane_tty: String,
    cols: u16,
    rows: u16,
    pane_active: bool,
    window_active: bool,
    pane_dead: bool,
    window_zoomed: bool,
    session_id: String,
    session_attached: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientInfo {
    tty: String,
    termname: String,
    termtype: String,
    cell_width: u16,
    cell_height: u16,
    control_mode: u8,
    utf8: bool,
    pid: u32,
}

fn query_pane(socket: &str, pane: &str, budget: Duration) -> Result<PaneInfo, TmuxError> {
    let out = run_tmux(
        socket,
        &["display-message", "-p", "-t", pane, PANE_FORMAT],
        budget,
    )?;
    parse_pane_info(out.trim_end_matches(['\n', '\r']))
}

fn parse_pane_info(line: &str) -> Result<PaneInfo, TmuxError> {
    let fields: Vec<&str> = line.split(FIELD_SEP).collect();
    if fields.len() != 11 {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!("pane query returned {} fields, expected 11", fields.len()),
        ));
    }
    let cols = parse_u16_field(fields[3], "pane_width")?;
    let rows = parse_u16_field(fields[4], "pane_height")?;
    let session_attached = fields[10].parse::<usize>().map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Query,
            format!("malformed session_attached {:?}", fields[10]),
        )
    })?;
    Ok(PaneInfo {
        version: fields[0].to_string(),
        pane_id: fields[1].to_string(),
        pane_tty: fields[2].to_string(),
        cols,
        rows,
        pane_active: fields[5] == "1",
        window_active: fields[6] == "1",
        pane_dead: fields[7] != "0",
        window_zoomed: fields[8] == "1",
        session_id: fields[9].to_string(),
        session_attached,
    })
}

fn query_clients(
    socket: &str,
    session: &str,
    budget: Duration,
) -> Result<Vec<ClientInfo>, TmuxError> {
    let out = run_tmux(
        socket,
        &["list-clients", "-t", session, "-F", CLIENT_FORMAT],
        budget,
    )?;
    parse_clients(&out)
}

fn parse_clients(out: &str) -> Result<Vec<ClientInfo>, TmuxError> {
    let mut clients = Vec::new();
    for line in out.lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(FIELD_SEP).collect();
        if fields.len() != 8 {
            return Err(TmuxError::new(
                TmuxFailureKind::Query,
                format!("client query returned {} fields, expected 8", fields.len()),
            ));
        }
        clients.push(ClientInfo {
            tty: fields[0].to_string(),
            termname: fields[1].to_string(),
            termtype: fields[2].to_string(),
            cell_width: parse_u16_field(fields[3], "client_cell_width")?,
            cell_height: parse_u16_field(fields[4], "client_cell_height")?,
            control_mode: fields[5].parse::<u8>().unwrap_or(1),
            utf8: fields[6] != "0",
            pid: fields[7].parse::<u32>().map_err(|_| {
                TmuxError::new(
                    TmuxFailureKind::Query,
                    format!("malformed client_pid {:?}", fields[7]),
                )
            })?,
        });
    }
    Ok(clients)
}

fn query_passthrough(socket: &str, pane: &str, budget: Duration) -> Result<Passthrough, TmuxError> {
    let out = run_tmux(
        socket,
        &[
            "show-options",
            "-A",
            "-p",
            "-v",
            "-t",
            pane,
            "allow-passthrough",
        ],
        budget,
    )?;
    let value = out.trim();
    if value.is_empty() {
        // tmux without a pane-local or inherited value defaults to off.
        return Ok(Passthrough::Off);
    }
    Passthrough::parse(value).ok_or_else(|| {
        TmuxError::new(
            TmuxFailureKind::Query,
            format!("unrecognized allow-passthrough value {value:?}"),
        )
    })
}

fn query_alternate_screen(socket: &str, pane: &str, budget: Duration) -> Result<bool, TmuxError> {
    let out = run_tmux(
        socket,
        &[
            "show-options",
            "-A",
            "-p",
            "-v",
            "-t",
            pane,
            "alternate-screen",
        ],
        budget,
    )?;
    match out.trim() {
        "on" => Ok(true),
        "off" => Ok(false),
        "" => Ok(true),
        value => Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!("unrecognized alternate-screen value {value:?}"),
        )),
    }
}

fn query_truecolor(socket: &str, termname: &str, budget: Duration) -> Result<bool, TmuxError> {
    // Ghostty's terminfo advertises Tc, so the default for xterm-ghostty is
    // truecolor; an explicit matching override can disable or re-enable it.
    let mut enabled = true;
    for option in ["terminal-overrides", "terminal-features"] {
        let out = run_tmux(socket, &["show-options", "-s", option], budget)?;
        apply_truecolor_entries(&out, termname, &mut enabled);
    }
    Ok(enabled)
}

fn apply_truecolor_entries(out: &str, termname: &str, enabled: &mut bool) {
    for line in out.lines() {
        let entry = line.split_once(' ').map_or(line, |(_, value)| value);
        let Some((pattern, caps)) = entry.split_once(':') else {
            continue;
        };
        if !glob_match(pattern.trim(), termname) {
            continue;
        }
        for cap in caps.split(':') {
            match cap {
                "Tc" | "RGB" => *enabled = true,
                "Tc@" | "RGB@" => *enabled = false,
                _ => {}
            }
        }
    }
}

fn parse_u16_field(value: &str, name: &str) -> Result<u16, TmuxError> {
    value.parse::<u16>().map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Query,
            format!("malformed {name} {value:?}"),
        )
    })
}

/// Run one fixed tmux command with no shell, bounded output, and a hard
/// deadline. Read-only by construction (the call sites only display/list/show).
fn run_tmux(socket: &str, args: &[&str], budget: Duration) -> Result<String, TmuxError> {
    let mut child = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("-f")
        .arg("/dev/null")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| TmuxError::new(TmuxFailureKind::Query, format!("cannot run tmux: {err}")))?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                return Err(TmuxError::new(
                    TmuxFailureKind::Query,
                    format!("cannot wait for tmux: {err}"),
                ));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TmuxError::new(
                TmuxFailureKind::Query,
                format!("tmux query timed out after {} ms", budget.as_millis()),
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
    let mut output = child.wait_with_output().map_err(|err| {
        TmuxError::new(
            TmuxFailureKind::Query,
            format!("cannot read tmux output: {err}"),
        )
    })?;
    if output.stdout.len() > MAX_QUERY_BYTES || output.stderr.len() > MAX_QUERY_BYTES {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            "tmux query exceeded the response bound",
        ));
    }
    if !output.status.success() {
        output.stderr.truncate(512);
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!("tmux query failed: {}", detail.trim()),
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| {
        TmuxError::new(
            TmuxFailureKind::Query,
            "tmux query returned non-UTF-8 output",
        )
    })?;
    if text.contains('\0') {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            "tmux query returned NUL bytes",
        ));
    }
    Ok(text)
}

fn apply_pane_info(pane: &mut TmuxPane, info: PaneInfo) -> Result<(), TmuxError> {
    if info.pane_id != pane.pane_id || info.pane_tty != pane.pane_tty {
        return Err(TmuxError::new(
            TmuxFailureKind::Query,
            format!(
                "pane target {} now resolves to {} ({}); the session changed",
                pane.pane_id, info.pane_id, info.pane_tty
            ),
        ));
    }
    pane.cols = info.cols;
    pane.rows = info.rows;
    pane.session_id = info.session_id;
    pane.tmux_version = info.version;
    pane.pane_active = info.pane_active;
    pane.window_active = info.window_active;
    pane.window_zoomed = info.window_zoomed;
    pane.pane_dead = info.pane_dead;
    pane.attached_clients = info.session_attached;
    pane.visible = visible(
        pane.attached_clients,
        pane.window_active,
        pane.window_zoomed,
        pane.pane_active,
        pane.pane_dead,
    );
    Ok(())
}

fn apply_client(pane: &mut TmuxPane, client: &ClientInfo) -> Result<(), TmuxError> {
    validate_client(client)?;
    pane.attached_clients = 1;
    pane.client_tty = client.tty.clone();
    pane.client_termname = client.termname.clone();
    pane.client_termtype = client.termtype.clone();
    pane.client_pid = client.pid;
    pane.cell_width = client.cell_width;
    pane.cell_height = client.cell_height;
    pane.visible = visible(
        pane.attached_clients,
        pane.window_active,
        pane.window_zoomed,
        pane.pane_active,
        pane.pane_dead,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Protocol builders
// ---------------------------------------------------------------------------

/// Wrap one inner escape sequence in tmux's DCS passthrough form. Every
/// embedded ESC byte is doubled exactly as tmux 3.5a requires; tmux strips the
/// wrapper and restores single ESC bytes before writing to the outer terminal.
pub fn wrap_dcs(inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(inner.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &byte in inner {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// One quiet image deletion, wrapped for tmux. `d=I` removes the image and all
/// of its virtual placements by id, so another pane's image is never touched.
pub fn delete_command(image_id: u32) -> Vec<u8> {
    wrap_dcs(format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").as_bytes())
}

/// The Unicode placeholder grid that makes the virtual placement visible.
///
/// The first line erases the pane-local alternate screen and homes the cursor;
/// each content row then carries `U+10EEEE` plus the row and column
/// diacritics, under a foreground color encoding the 24-bit image id. tmux
/// stores these as ordinary styled cells, clips them to the pane, and repaints
/// them on redraw.
pub fn placeholder_grid(image_id: u32, cols: u16, rows: u16) -> Result<Vec<u8>, TmuxError> {
    pos_grid(image_id, cols, rows)
}

fn pos_grid(image_id: u32, cols: u16, rows: u16) -> Result<Vec<u8>, TmuxError> {
    if image_id == 0 || image_id > MAX_IMAGE_ID {
        return Err(TmuxError::new(
            TmuxFailureKind::ImageId,
            format!("image id {image_id:#08x} does not fit a 24-bit placeholder color"),
        ));
    }
    let cols = usize::from(cols);
    let rows = usize::from(rows);
    if cols == 0 || rows == 0 || cols > DIACRITICS.len() || rows > DIACRITICS.len() {
        return Err(TmuxError::new(
            TmuxFailureKind::GridTooLarge,
            format!("placeholder grid {cols}x{rows} exceeds the 297-entry diacritic table"),
        ));
    }
    let worst_case = rows
        .saturating_mul(cols.saturating_mul(12).saturating_add(12))
        .saturating_add(128);
    if worst_case > MAX_GRID_OUTPUT_BYTES {
        return Err(TmuxError::new(
            TmuxFailureKind::GridTooLarge,
            format!("placeholder grid {cols}x{rows} exceeds the output bound"),
        ));
    }

    let red = (image_id >> 16) & 0xff;
    let green = (image_id >> 8) & 0xff;
    let blue = image_id & 0xff;
    let mut out = String::with_capacity(worst_case);
    out.push_str("\x1b[2J\x1b[H");
    out.push_str(&format!("\x1b[38;2;{red};{green};{blue}m"));
    let mut buffer = [0u8; 4];
    for row in 0..rows {
        out.push_str(&format!("\x1b[{};1H", row + 1));
        let row_diacritic = char::from_u32(DIACRITICS[row]).expect("table code points are valid");
        for column in 0..cols {
            let column_diacritic =
                char::from_u32(DIACRITICS[column]).expect("table code points are valid");
            out.push('\u{10EEEE}');
            out.push_str(row_diacritic.encode_utf8(&mut buffer));
            out.push_str(column_diacritic.encode_utf8(&mut buffer));
        }
    }
    out.push_str("\x1b[39m");
    Ok(out.into_bytes())
}

/// Session-unique 24-bit image id for the placeholder path. Mixes the process
/// id with the clock so simultaneous Glisk panes cannot collide; the 24-bit
/// ceiling is the placeholder foreground encoding.
pub fn session_image_id() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() ^ elapsed.as_secs() as u32)
        .unwrap_or(0);
    let pid = std::process::id();
    let mut id = (pid.wrapping_mul(0x9E37_79B1) ^ nanos.rotate_left(13)) & MAX_IMAGE_ID;
    if id == 0 || id == PROBE_IMAGE_ID {
        id = 0x474c_49;
    }
    id
}

// ---------------------------------------------------------------------------
// Small parsers shared with tests
// ---------------------------------------------------------------------------

fn parse_version(value: &str) -> Option<(u32, u32)> {
    let value = value.trim();
    let value = value.strip_prefix("next-").unwrap_or(value);
    let value = value.split(['-', ' ']).next().unwrap_or(value);
    let mut parts = value.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts
        .next()?
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse::<u32>()
        .ok()?;
    Some((major, minor))
}

fn parse_ghostty_version(termtype: &str) -> Option<(u32, u32)> {
    parse_version(termtype.strip_prefix("ghostty")?.trim())
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut retry) = (None, 0);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            retry = ti;
            pi += 1;
        } else if let Some(star_pi) = star {
            pi = star_pi + 1;
            retry += 1;
            ti = retry;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dcs_passthrough_doubles_embedded_escapes() {
        let inner = b"\x1b_Ga=T,i=7;AAAA\x1b\\";
        let wrapped = wrap_dcs(inner);
        assert_eq!(wrapped, b"\x1bPtmux;\x1b\x1b_Ga=T,i=7;AAAA\x1b\x1b\\\x1b\\");
        assert_eq!(wrapped.first(), Some(&0x1b));
        assert_eq!(wrapped.get(1), Some(&b'P'));
        assert_eq!(wrapped.iter().filter(|b| **b == 0x1b).count(), 6);
    }

    #[test]
    fn deletion_targets_only_the_owned_image() {
        let wire = delete_command(0x123456);
        assert!(wire.starts_with(b"\x1bPtmux;"));
        assert!(
            wire.windows(b"a=d,d=I,i=1193046,q=2".len())
                .any(|w| w == b"a=d,d=I,i=1193046,q=2")
        );
        assert!(!String::from_utf8_lossy(&wire).contains("d=A"));
    }

    #[test]
    fn placeholder_grid_encodes_row_column_and_color() {
        let grid = placeholder_grid(0x010203, 3, 2).expect("grid");
        let text = String::from_utf8(grid).expect("utf8");
        assert!(text.starts_with("\x1b[2J\x1b[H\x1b[38;2;1;2;3m"));
        assert!(text.ends_with("\x1b[39m"));
        assert_eq!(text.matches('\u{10EEEE}').count(), 6);
        assert!(text.contains("\x1b[1;1H"));
        assert!(text.contains("\x1b[2;1H"));
        // Row 0, column 0, then columns 1 and 2.
        assert!(text.contains(
            "\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\u{10EEEE}\u{0305}\u{030E}"
        ));
        // Row 1, column 0.
        assert!(text.contains("\x1b[2;1H\u{10EEEE}\u{030D}\u{0305}"));
    }

    #[test]
    fn placeholder_grid_rejects_unencodable_inputs() {
        assert!(placeholder_grid(0, 1, 1).is_err());
        assert!(placeholder_grid(MAX_IMAGE_ID + 1, 1, 1).is_err());
        assert!(placeholder_grid(1, 0, 1).is_err());
        assert!(placeholder_grid(1, 1, 0).is_err());
        assert!(placeholder_grid(1, 298, 1).is_err());
        assert!(placeholder_grid(1, 1, 298).is_err());
    }

    #[test]
    fn tmux_environment_is_parsed_strictly() {
        assert_eq!(
            parse_tmux_env("/private/tmp/tmux-501/default,1611,2").unwrap(),
            ("/private/tmp/tmux-501/default".to_string(), 1611, 2)
        );
        assert!(parse_tmux_env("/tmp/default,1").is_err());
        assert!(parse_tmux_env("relative,1,2").is_err());
        assert!(validate_pane_id("%101").is_ok());
        assert!(validate_pane_id("101").is_err());
        assert!(validate_pane_id("%").is_err());
        assert!(validate_pane_id("%12x").is_err());
    }

    #[test]
    fn versions_are_compared_by_major_minor() {
        assert_eq!(parse_version("3.5a"), Some((3, 5)));
        assert_eq!(parse_version("next-3.6"), Some((3, 6)));
        assert_eq!(parse_version("openbsd-7.5"), None);
        assert_eq!(parse_ghostty_version("ghostty 1.4.0"), Some((1, 4)));
        assert_eq!(parse_ghostty_version("ghostty 1.4.0-abc"), Some((1, 4)));
        assert_eq!(parse_ghostty_version("ghostty 1.3.1"), Some((1, 3)));
        assert_eq!(parse_ghostty_version("kitty 0.40"), None);
    }

    #[test]
    fn pane_and_client_outputs_parse() {
        let pane_line = [
            "3.5a",
            "%101",
            "/dev/ttys007",
            "92",
            "25",
            "1",
            "1",
            "0",
            "0",
            "$2",
            "1",
        ]
        .join("|");
        let info = parse_pane_info(&pane_line).unwrap();
        assert_eq!(info.version, "3.5a");
        assert_eq!(info.pane_id, "%101");
        assert_eq!((info.cols, info.rows), (92, 25));
        assert!(info.pane_active && info.window_active);
        assert!(!info.window_zoomed && !info.pane_dead);
        assert_eq!(info.session_attached, 1);

        let client_line = [
            "/dev/ttys013",
            "xterm-ghostty",
            "ghostty 1.4.0",
            "17",
            "34",
            "0",
            "1",
            "12504",
        ]
        .join("|");
        let clients = parse_clients(&format!("{client_line}\n")).unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].cell_width, 17);
        assert_eq!(clients[0].cell_height, 34);
        assert!(clients[0].utf8);
        assert_eq!(clients[0].control_mode, 0);
        assert_eq!(clients[0].pid, 12504);
    }

    #[test]
    fn truecolor_overrides_are_ordered() {
        let mut enabled = true;
        apply_truecolor_entries(
            "terminal-overrides[0] xterm*:Tc\n",
            "xterm-ghostty",
            &mut enabled,
        );
        assert!(enabled);
        apply_truecolor_entries(
            "terminal-overrides[1] xterm-ghostty:Tc@\n",
            "xterm-ghostty",
            &mut enabled,
        );
        assert!(!enabled);
        apply_truecolor_entries(
            "terminal-features[0] xterm*:RGB\n",
            "xterm-ghostty",
            &mut enabled,
        );
        assert!(enabled);
        assert!(glob_match("xterm*", "xterm-ghostty"));
        assert!(glob_match("*ghostty", "xterm-ghostty"));
        assert!(!glob_match("screen*", "xterm-ghostty"));
    }

    #[test]
    fn session_image_ids_fit_the_placeholder_color() {
        for _ in 0..100 {
            let id = session_image_id();
            assert!(id > 0);
            assert!(id <= MAX_IMAGE_ID);
            assert_ne!(id, PROBE_IMAGE_ID);
        }
    }

    #[test]
    fn errors_are_actionable() {
        let off = TmuxError::new(
            TmuxFailureKind::PassthroughOff,
            "graphics passthrough is off for pane %1; enable it in that pane with \
             `tmux set-option -p allow-passthrough on`",
        );
        assert!(off.to_string().contains("allow-passthrough"));
        assert!(off.to_string().contains("set-option -p"));
    }
}
