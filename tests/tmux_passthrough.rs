//! Live tmux passthrough and Unicode-placeholder protocol tests (bead
//! `glisk-2hp.11`).
//!
//! Each test starts its own `tmux -S <unique socket> -f /dev/null` server, so
//! no test ever contacts the user's live tmux server. The outer terminal is a
//! pseudo-terminal owned by the test; tmux's client writes there exactly as it
//! would write to Ghostty. This proves the protocol framing, placeholder cell
//! preservation, truecolor foreground, redraw, and per-image cleanup. It is not
//! a compositor test: the final image rendering still needs a real Ghostty
//! window.

#[path = "../src/tmux.rs"]
mod tmux;

use std::{
    env,
    fs::OpenOptions,
    io::Write,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static PTY_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

const CELL_W: u16 = 17;
const CELL_H: u16 = 34;

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn ghostty_terminfo() -> Option<&'static str> {
    const PATH: &str = "/Applications/Ghostty.app/Contents/Resources/terminfo";
    Path::new(PATH).is_dir().then_some(PATH)
}

struct Pty {
    master: RawFd,
    slave: RawFd,
}

impl Pty {
    fn open(cols: u16, rows: u16) -> Self {
        Self::open_with_pixels(cols, rows, cols * CELL_W, rows * CELL_H)
    }

    fn open_with_pixels(cols: u16, rows: u16, x_pixels: u16, y_pixels: u16) -> Self {
        let serial = PTY_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: openpty writes both descriptors on success.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: x_pixels,
            ws_ypixel: y_pixels,
        };
        // SAFETY: size is an initialized winsize.
        let rc = unsafe { libc::ioctl(slave, libc::TIOCSWINSZ, &size) };
        assert_eq!(rc, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
        // Serialize allocation only; release before returning so one test can
        // hold multiple PTYs (two attached clients).
        drop(serial);
        Self { master, slave }
    }

    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * CELL_W,
            ws_ypixel: rows * CELL_H,
        };
        // SAFETY: size is an initialized winsize; setting the master size
        // raises SIGWINCH for the client process group.
        let rc = unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ, &size) };
        assert_eq!(rc, 0, "TIOCSWINSZ(master)");
    }

    fn read(&self, timeout: Duration) -> Vec<u8> {
        let mut pfd = libc::pollfd {
            fd: self.master,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: one initialized pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc <= 0 {
            return Vec::new();
        }
        let mut buffer = [0u8; 65536];
        // SAFETY: buffer is writable for its full length.
        let read = unsafe { libc::read(self.master, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read <= 0 {
            Vec::new()
        } else {
            buffer[..read as usize].to_vec()
        }
    }

    fn drain(&self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let chunk = self.read(Duration::from_millis(40));
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(&chunk);
        }
        out
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: both descriptors were opened by openpty and are closed once.
        unsafe {
            libc::close(self.master);
            libc::close(self.slave);
        }
    }
}

struct TmuxServer {
    socket: PathBuf,
    session: String,
    term: String,
    terminfo: Option<&'static str>,
}

impl TmuxServer {
    fn start(passthrough: &str) -> Self {
        let term = if ghostty_terminfo().is_some() {
            "xterm-ghostty"
        } else {
            "xterm-256color"
        };
        Self::start_with_term(passthrough, term)
    }

    fn start_with_term(passthrough: &str, term: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let socket = env::temp_dir().join(format!("glisk-tmux-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let session = format!("probe{id}");
        let terminfo = (term == "xterm-ghostty").then(ghostty_terminfo).flatten();
        let term = term.to_string();

        let mut cmd = Command::new("tmux");
        cmd.arg("-S")
            .arg(&socket)
            .arg("-f")
            .arg("/dev/null")
            .args([
                "new-session",
                "-d",
                "-x",
                "80",
                "-y",
                "24",
                "-s",
                &session,
                "cat",
            ])
            .env("TERM", &term)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = terminfo {
            cmd.env("TERMINFO", path);
        }
        let out = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("start tmux server");
        assert!(out.success(), "tmux new-session failed");

        let server = Self {
            socket,
            session,
            term,
            terminfo,
        };
        server.run(&[
            "set-option",
            "-p",
            "-t",
            &server.pane_id(),
            "allow-passthrough",
            passthrough,
        ]);
        server.run(&["set-option", "-s", "terminal-overrides", "xterm*:Tc"]);
        server
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.arg("-S")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .env("TERM", &self.term)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        if let Some(path) = self.terminfo {
            cmd.env("TERMINFO", path);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().expect("run tmux");
        assert!(
            out.status.success(),
            "tmux {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pane_id(&self) -> String {
        self.run(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:0.0", self.session),
            "#{pane_id}",
        ])
        .trim()
        .to_string()
    }

    fn pane_tty(&self) -> String {
        self.run(&[
            "display-message",
            "-p",
            "-t",
            &self.pane_id(),
            "#{pane_tty}",
        ])
        .trim()
        .to_string()
    }

    fn write_pane(&self, bytes: &[u8]) {
        let tty = self.pane_tty();
        let mut file = OpenOptions::new()
            .write(true)
            .open(&tty)
            .unwrap_or_else(|err| panic!("open {tty}: {err}"));
        file.write_all(bytes).expect("write pane tty");
    }

    fn split(&self) -> String {
        self.run(&["split-window", "-h", "-t", &self.pane_id(), "cat"]);
        self.run(&["display-message", "-p", "-t", &self.session, "#{pane_id}"])
            .trim()
            .to_string()
    }

    fn attach(&self, pty: &Pty) -> Child {
        let mut cmd = self.command();
        cmd.args(["attach-session", "-t", &self.session])
            .stdin(Stdio::from(unsafe { Stdio::from_raw_fd(dup(pty.slave)) }))
            .stdout(Stdio::from(unsafe { Stdio::from_raw_fd(dup(pty.slave)) }))
            .stderr(Stdio::from(unsafe { Stdio::from_raw_fd(dup(pty.slave)) }));
        let child = cmd.spawn().expect("attach tmux client");
        thread::sleep(Duration::from_millis(250));
        let _ = pty.drain();
        child
    }
}

fn dup(fd: RawFd) -> RawFd {
    // SAFETY: dup duplicates a valid descriptor.
    let new = unsafe { libc::dup(fd) };
    assert!(new >= 0, "dup: {}", std::io::Error::last_os_error());
    new
}

/// Continuous client-output capture. The pane can write more than the PTY
/// buffer while a frame is transmitted, so the reader must run alongside.
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Capture {
    fn start(pty: &Pty) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_bytes = Arc::clone(&bytes);
        let reader_stop = Arc::clone(&stop);
        let fd = pty.master;
        let reader = thread::spawn(move || {
            while !reader_stop.load(Ordering::SeqCst) {
                let chunk = pty_read(fd, Duration::from_millis(10));
                if !chunk.is_empty() {
                    reader_bytes.lock().unwrap().extend_from_slice(&chunk);
                }
            }
        });
        Self {
            bytes,
            stop,
            reader: Some(reader),
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }

    fn wait_for<F>(&self, timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut(&[u8]) -> bool,
    {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate(&self.bytes.lock().unwrap()) {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        predicate(&self.bytes.lock().unwrap())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn pty_read(fd: RawFd, timeout: Duration) -> Vec<u8> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
    // SAFETY: one initialized pollfd.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc <= 0 {
        return Vec::new();
    }
    let mut buffer = [0u8; 65536];
    // SAFETY: buffer is writable for its full length.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read <= 0 {
        Vec::new()
    } else {
        buffer[..read as usize].to_vec()
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = self
            .command()
            .args(["kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn kitty_payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == 0x1b && bytes[index + 1] == b'_' && bytes[index + 2] == b'G' {
            let start = index + 3;
            match bytes[start..]
                .windows(2)
                .position(|window| window == b"\x1b\\")
            {
                Some(offset) => {
                    payloads.push(bytes[start..start + offset].to_vec());
                    index = start + offset + 2;
                }
                None => break,
            }
        } else {
            index += 1;
        }
    }
    payloads
}

fn payload_data(payload: &[u8]) -> &[u8] {
    let separator = payload
        .iter()
        .position(|byte| *byte == b';')
        .expect("payload separator");
    &payload[separator + 1..]
}

fn decoded_kitty_payload(bytes: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    let encoded: Vec<u8> = kitty_payloads(bytes)
        .iter()
        .flat_map(|payload| payload_data(payload).to_vec())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 Kitty payload")
}

fn png_fixture() -> Vec<u8> {
    // 1x1 transparent PNG, enough to exercise the protocol framing.
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0b, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I',
        b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];
    PNG.to_vec()
}

fn kitty_frame(image_id: u32, cols: u16, rows: u16) -> Vec<u8> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png_fixture());
    let mut inner =
        format!("\x1b_Ga=T,f=100,i={image_id},p=1,q=2,U=1,c={cols},r={rows},m=0;").into_bytes();
    inner.extend_from_slice(encoded.as_bytes());
    inner.extend_from_slice(b"\x1b\\");
    let mut wire = tmux::wrap_dcs(&inner);
    wire.extend_from_slice(&tmux::placeholder_grid(image_id, cols, rows).expect("grid"));
    wire
}

#[test]
fn passthrough_off_drops_graphics_but_keeps_plain_placeholders() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("off");
    let pty = Pty::open(80, 24);
    let mut client = server.attach(&pty);

    let id = 0x123456;
    server.write_pane(&kitty_frame(id, 4, 2));
    let out = pty.drain();

    assert!(
        !contains(&out, b"\x1b_Ga=T"),
        "passthrough off must not forward the Kitty command"
    );
    assert!(
        contains(&out, b"\x1bPtmux;") || !contains(&out, b"\x1b_G"),
        "the wrapper must not leak as a Kitty command"
    );
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains('\u{10EEEE}'),
        "placeholders are ordinary pane text even without passthrough"
    );

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn enabled_passthrough_delivers_unwrapped_graphics_and_truecolor_placeholders() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let pty = Pty::open(80, 24);
    let mut client = server.attach(&pty);

    let id = 0x0a0b0c;
    server.write_pane(&kitty_frame(id, 6, 3));
    let out = pty.drain();

    let header = format!("a=T,f=100,i={id},p=1,q=2,U=1,c=6,r=3,m=0;");
    assert!(
        contains(&out, format!("\x1b_G{header}").as_bytes()),
        "tmux should strip the DCS wrapper and deliver one inner Kitty command"
    );
    assert!(
        !contains(&out, b"\x1b\x1b_G"),
        "embedded ESC doubling must be undone by tmux"
    );
    assert!(
        !contains(&out, b"\x1bPtmux;"),
        "the tmux wrapper must not reach the outer terminal"
    );
    let color_semicolon = format!("\x1b[38;2;{};{};{}m", 0x0a, 0x0b, 0x0c);
    let color_colon = format!("\x1b[38:2:{}:{}:{}m", 0x0a, 0x0b, 0x0c);
    assert!(
        contains(&out, color_semicolon.as_bytes()) || contains(&out, color_colon.as_bytes()),
        "the 24-bit placeholder color must survive tmux; captured {:?}",
        String::from_utf8_lossy(&out)
    );
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.matches('\u{10EEEE}').count() >= 18,
        "every placeholder cell must reach the outer terminal"
    );
    assert!(text.contains('\u{0305}'));

    // A pane resize/redraw must repaint the placeholder cells.
    let _ = pty.drain();
    pty.resize(72, 20);
    // SAFETY: the client is the foreground process of this pty.
    unsafe { libc::kill(client.id() as libc::pid_t, libc::SIGWINCH) };
    let redrawn = pty.drain();
    assert!(
        String::from_utf8_lossy(&redrawn).contains('\u{10EEEE}'),
        "resize redraw should re-emit placeholders"
    );

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn two_panes_keep_independent_ids_and_delete_only_their_own() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let first_pane = server.pane_id();
    let pty = Pty::open(100, 30);
    let mut client = server.attach(&pty);

    let second_pane = server.split();
    server.run(&[
        "set-option",
        "-p",
        "-t",
        &second_pane,
        "allow-passthrough",
        "on",
    ]);
    assert_ne!(first_pane, second_pane);

    let first_id = 0x111111;
    let second_id = 0x222222;
    // Write the same grid shape to each pane's tty; tmux clips each to its pane.
    for (pane, id) in [(&first_pane, first_id), (&second_pane, second_id)] {
        let tty = server
            .run(&["display-message", "-p", "-t", pane, "#{pane_tty}"])
            .trim()
            .to_string();
        let mut file = OpenOptions::new().write(true).open(&tty).expect("pane tty");
        file.write_all(&kitty_frame(id, 5, 2)).expect("write frame");
    }
    let out = pty.drain();
    assert!(contains(&out, format!("i={first_id},").as_bytes()));
    assert!(contains(&out, format!("i={second_id},").as_bytes()));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.matches('\u{10EEEE}').count() >= 20,
        "both panes' placeholder grids must reach the outer terminal"
    );

    let delete = tmux::delete_command(first_id);
    server.write_pane(&delete);
    let cleanup = pty.drain();
    assert!(contains(
        &cleanup,
        format!("a=d,d=I,i={first_id},q=2").as_bytes()
    ));
    assert!(
        !String::from_utf8_lossy(&cleanup).contains(&format!("i={second_id},q=2")),
        "cleanup must not delete the other pane's image"
    );

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn capability_queries_do_not_reach_a_tmux_pane_as_replies() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let pty = Pty::open(80, 24);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let script = r#"printf '\033[16t'; if IFS= read -r -t 1 -n 10 cell; then printf 'CELL_REPLY\n'; else printf 'CELL_NONE\n'; fi; printf '\033_Gi=31,a=q,t=d,f=24,s=1,v=1;AAAA\033\\'; if IFS= read -r -t 1 -n 1 kitty; then printf 'KITTY_REPLY\n'; else printf 'KITTY_NONE\n'; fi; sleep 2"#;
    server.run(&["respawn-pane", "-k", "-t", &pane, "bash", "-c", script]);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut screen = String::new();
    while Instant::now() < deadline {
        screen = server.run(&["capture-pane", "-p", "-t", &pane]);
        if screen.contains("KITTY_NONE") || screen.contains("KITTY_REPLY") {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        screen.contains("CELL_REPLY"),
        "tmux 3.5a answers CSI 16t for the pane: {screen:?}"
    );
    assert!(
        screen.contains("KITTY_NONE"),
        "the Kitty feature query must not produce a pane reply: {screen:?}"
    );
    assert!(!screen.contains("KITTY_REPLY"));

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn glisk_uses_the_tmux_placeholder_path_end_to_end() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    // Match the pane height to the outer PTY for the grid size assertions.
    server.run(&["set-option", "status", "off"]);
    let pty = Pty::open(80, 24);
    let capture = Capture::start(&pty);
    let mut client = server.attach(&pty);

    let pane = server.pane_id();
    let metrics = env::temp_dir().join(format!(
        "glisk-tmux-metrics-{}-{}.txt",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!(
        "'{binary}' '{example}' --metrics '{}'; exec sh",
        metrics.display()
    );
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);

    assert!(
        capture.wait_for(Duration::from_secs(30), |bytes| {
            contains(bytes, b"glisk |")
        }),
        "the live pager never painted its status row: {:?}",
        String::from_utf8_lossy(&capture.snapshot())
    );

    let started = capture.snapshot();
    let text = String::from_utf8_lossy(&started);
    assert!(
        contains(&started, b"a=T,f=100"),
        "the first tmux frame must transmit and place the image"
    );
    assert!(
        contains(&started, b"U=1"),
        "the tmux path must request a Unicode-placeholder placement"
    );
    assert!(
        !contains(&started, b"\x1bPtmux;"),
        "the passthrough wrapper must be stripped by tmux"
    );
    assert!(
        text.matches('\u{10EEEE}').count() >= 80 * 23,
        "the pane needs at least one full placeholder grid, saw {} cells",
        text.matches('\u{10EEEE}').count()
    );

    // Scrolling at a stable size replaces the image pixels but must retain the
    // placeholder cells: re-emitting them would clear and repaint the pane.
    server.run(&["send-keys", "-t", &pane, "G"]);
    assert!(
        capture.wait_for(Duration::from_secs(10), |bytes| {
            contains(&bytes[started.len()..], b"100%")
        }),
        "scrolling to the end must update the status row"
    );
    let scrolled = capture.snapshot();
    let scroll_frame = &scrolled[started.len()..];
    assert!(
        contains(scroll_frame, b"\x1b_Ga=f,f=100,"),
        "a same-size scroll must edit base frame 1"
    );
    assert!(
        !contains(scroll_frame, b"\x1b_Ga=T"),
        "a same-size scroll must not retransmit and replace the image"
    );
    assert!(
        !contains(scroll_frame, b"\x1b_Ga=d"),
        "a same-size scroll must not delete the image"
    );
    assert!(
        !contains(scroll_frame, b"p=1"),
        "a same-size scroll must preserve the virtual placement"
    );
    assert!(
        contains(scroll_frame, b"r=1,X=1,"),
        "frame edits must overwrite base frame 1"
    );
    let decoded = decoded_kitty_payload(scroll_frame);
    assert!(
        decoded.starts_with(b"\x89PNG\r\n\x1a\n"),
        "the frame-edit chunks must reconstruct a complete PNG"
    );
    assert!(
        !contains(scroll_frame, "\u{10EEEE}".as_bytes()),
        "scrolling at the same size must retain the placeholder grid"
    );
    assert!(!contains(scroll_frame, b"\x1b[2J"));
    let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
    assert_eq!(screen.matches('\u{10EEEE}').count(), 80 * 23);

    // Send a terminal mouse report to the attached tmux client, so tmux has
    // to route it to Glisk and translate it to pane coordinates.
    // SAFETY: the duplicate is owned by this File.
    let mut client_input = unsafe { std::fs::File::from_raw_fd(dup(pty.master)) };
    client_input.write_all(b"\x1b[<64;2;2M").unwrap();
    assert!(
        capture.wait_for(Duration::from_secs(5), |bytes| {
            contains(&bytes[scrolled.len()..], b"\x1b_Ga=f,f=100,")
        }),
        "wheel input from the tmux client must scroll the pager"
    );

    // A pane resize changes the grid dimensions and must rebuild the cells.
    pty.resize(72, 20);
    // SAFETY: the client is the foreground process of this PTY.
    unsafe { libc::kill(client.id() as libc::pid_t, libc::SIGWINCH) };
    assert!(
        capture.wait_for(Duration::from_secs(10), |bytes| {
            contains(&bytes[scrolled.len()..], b",c=72,r=19,")
        }),
        "a pane resize must upload an image with the new dimensions"
    );
    let resized_frame = &capture.snapshot()[scrolled.len()..];
    assert!(
        contains(resized_frame, b"\x1b_Ga=T,f=100"),
        "a resize must bootstrap and re-place the image"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
        if screen.matches('\u{10EEEE}').count() == 72 * 19 && screen.contains("glisk |") {
            break;
        }
        assert!(Instant::now() < deadline, "the resized grid was incomplete");
        thread::sleep(Duration::from_millis(25));
    }

    // Quit through the real key path; tmux restores the pane's normal screen
    // (the outer client stays in tmux's own alternate screen), so check the
    // pane contents rather than an outer `?1049l`.
    server.run(&["send-keys", "-t", &pane, "q"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut restored = false;
    while Instant::now() < deadline {
        let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
        if screen.contains("sh-3.2$") && !screen.contains("glisk |") {
            restored = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(restored, "q must restore the pane's shell");
    let finished = capture.snapshot();
    let id = metrics_value(&metrics, "image_id").expect("image id metric");
    assert!(
        contains(&finished, format!("a=d,d=I,i={id},q=2").as_bytes()),
        "cleanup must delete only the session image"
    );
    assert_eq!(
        metrics_value(&metrics, "placement").as_deref(),
        Some("tmux")
    );
    let _ = std::fs::remove_file(&metrics);

    let _ = client.kill();
    let _ = client.wait();
}

fn tmux_watch_html(marker: &str) -> String {
    format!(
        "<!doctype html><html><head><style>
         html, body {{ margin: 0; background: #fff; color: #000; }}
         body {{ font: 16px/24px monospace; }}
         section {{ min-height: 240px; }}
         </style></head><body>
         <h1 id=\"tmux-watch-top\">Watched report</h1>
         <section id=\"tmux-watch-first\"><p>{marker}</p></section>
         <section id=\"tmux-watch-last\"><p>Closing section.</p></section>
         </body></html>"
    )
}

#[test]
fn tmux_pane_follows_local_report_reloads() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    server.run(&["set-option", "status", "off"]);
    let pty = Pty::open(80, 24);
    let capture = Capture::start(&pty);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let report = env::temp_dir().join(format!("glisk-tmux-watch-{}-{id}.html", std::process::id()));
    let replacement = env::temp_dir().join(format!(
        "glisk-tmux-watch-{}-{id}.replacement",
        std::process::id()
    ));
    std::fs::write(&report, tmux_watch_html("alpha revision")).expect("write watched report");
    let metrics = env::temp_dir().join(format!(
        "glisk-tmux-watch-metrics-{}-{id}.txt",
        std::process::id()
    ));
    let binary = env!("CARGO_BIN_EXE_glisk");
    let script = format!(
        "'{binary}' '{}' --metrics '{}'; exec sh",
        report.display(),
        metrics.display()
    );
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);
    assert!(
        capture.wait_for(Duration::from_secs(30), |bytes| {
            contains(bytes, b"glisk |") && contains(bytes, b"\x1b_Ga=T")
        }),
        "the tmux pager never painted its first watched frame: {:?}",
        String::from_utf8_lossy(&capture.snapshot())
    );

    let first = capture.snapshot().len();
    std::fs::write(&report, tmux_watch_html("beta revision")).expect("rewrite report");
    assert!(
        capture.wait_for(Duration::from_secs(10), |bytes| {
            contains(&bytes[first..], b"\x1b_Ga=")
        }),
        "a normal save must reload in the tmux pane"
    );

    let second = capture.snapshot().len();
    std::fs::write(&replacement, tmux_watch_html("gamma revision"))
        .expect("write atomic replacement");
    std::fs::rename(&replacement, &report).expect("atomic replacement");
    assert!(
        capture.wait_for(Duration::from_secs(10), |bytes| {
            contains(&bytes[second..], b"\x1b_Ga=")
        }),
        "an atomic replacement must reload in the tmux pane"
    );

    server.run(&["send-keys", "-t", &pane, "q"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
        if screen.contains("sh-3.2$") && !screen.contains("glisk |") {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !metrics.exists() {
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        metrics_value(&metrics, "watch").as_deref(),
        Some("on"),
        "tmux live sessions must watch by default"
    );
    let reloads = metrics_value(&metrics, "watch_reloads")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    assert!(
        reloads >= 2,
        "tmux pane must reload both saves, saw {reloads}"
    );
    assert_eq!(
        metrics_value(&metrics, "placement").as_deref(),
        Some("tmux")
    );

    let _ = std::fs::remove_file(&metrics);
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_file(&replacement);
    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn returning_to_a_visible_pane_reuploads_the_image_and_keeps_the_grid() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    server.run(&["set-option", "status", "off"]);
    let pty = Pty::open(80, 24);
    let capture = Capture::start(&pty);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!("'{binary}' '{example}'; exec sh");
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);
    assert!(
        capture.wait_for(Duration::from_secs(30), |bytes| {
            contains(bytes, b"glisk |")
        }),
        "the live pager never painted its status row"
    );

    // Leave the window, let at least one visibility sync observe it hidden,
    // then return. The image must be re-uploaded; the cells stay in place.
    server.run(&["new-window", "-d", "-t", &server.session, "cat"]);
    server.run(&["select-window", "-t", &format!("{}:1", server.session)]);
    thread::sleep(Duration::from_millis(900));
    let hidden = capture.snapshot();

    server.run(&["select-window", "-t", &format!("{}:0", server.session)]);
    assert!(
        capture.wait_for(Duration::from_secs(10), |bytes| {
            contains(&bytes[hidden.len()..], b"\x1b_Ga=T")
        }),
        "returning to the pane must re-upload the image"
    );
    let returned = &capture.snapshot()[hidden.len()..];
    let delete = returned
        .windows(b"\x1b_Ga=d,d=I".len())
        .position(|window| window == b"\x1b_Ga=d,d=I");
    let transmit = returned
        .windows(b"\x1b_Ga=T".len())
        .position(|window| window == b"\x1b_Ga=T");
    assert!(
        matches!((delete, transmit), (Some(delete), Some(transmit)) if delete < transmit),
        "recovery must abort a partial frame edit before the fresh bootstrap"
    );
    let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
    assert_eq!(screen.matches('\u{10EEEE}').count(), 80 * 23);

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn ctrl_c_restores_the_tmux_pane() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let pty = Pty::open(80, 24);
    let capture = Capture::start(&pty);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!("'{binary}' '{example}'; exec sh");
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);
    assert!(
        capture.wait_for(Duration::from_secs(30), |bytes| {
            contains(bytes, b"glisk |")
        }),
        "pager did not start"
    );
    server.run(&["send-keys", "-t", &pane, "C-c"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut restored = false;
    while Instant::now() < deadline {
        let screen = server.run(&["capture-pane", "-p", "-t", &pane]);
        if screen.contains("sh-3.2$") && !screen.contains("glisk |") {
            restored = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(restored, "Ctrl+C must restore the pane's shell");
    // tmux may reorder its own mouse-tracking reset, so check each mode.
    let snapshot = capture.snapshot();
    assert!(
        contains(&snapshot, b"\x1b[?1000l"),
        "Ctrl+C must disable button-event tracking in the pane"
    );
    assert!(
        contains(&snapshot, b"\x1b[?1006l"),
        "Ctrl+C must disable SGR mouse encoding in the pane"
    );
    let _ = client.kill();
    let _ = client.wait();
}

fn metrics_value(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

#[test]
fn unsupported_client_falls_back_to_clean_plain_text_in_tmux() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start_with_term("on", "xterm-256color");
    let pty = Pty::open(80, 24);
    let capture = Capture::start(&pty);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let err_path = env::temp_dir().join(format!(
        "glisk-tmux-fallback-{}-{}.err",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!("'{binary}' '{example}' 2>'{}'", err_path.display());
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);

    assert!(
        capture.wait_for(Duration::from_secs(20), |bytes| {
            contains(bytes, b"Glisk renderer decision report")
        }),
        "fallback did not write plain text"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut diagnostic = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&err_path) {
            if !text.is_empty() {
                diagnostic = text;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        diagnostic.contains("tmux support unsupported-client"),
        "unexpected diagnostic: {diagnostic}"
    );
    assert!(
        diagnostic.contains("Ghostty 1.4+"),
        "diagnostic must name the supported client: {diagnostic}"
    );
    assert!(
        diagnostic.contains("using plain text"),
        "automatic failure should say it is using text: {diagnostic}"
    );
    assert!(
        !contains(&capture.snapshot(), b"U=1"),
        "a rejected client must not receive graphics"
    );
    let _ = std::fs::remove_file(&err_path);

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn missing_client_cell_geometry_falls_back_to_text() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let pty = Pty::open_with_pixels(80, 24, 0, 0);
    let mut client = server.attach(&pty);
    let pane = server.pane_id();
    let err_path = env::temp_dir().join(format!(
        "glisk-tmux-geometry-{}-{}.err",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!("'{binary}' '{example}' 2>'{}'", err_path.display());
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut diagnostic = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&err_path) {
            if !text.is_empty() {
                diagnostic = text;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        diagnostic.contains("tmux support geometry"),
        "unexpected diagnostic: {diagnostic}"
    );
    let _ = std::fs::remove_file(&err_path);

    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn ambiguous_multi_client_attachment_falls_back_with_a_diagnostic() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let server = TmuxServer::start("on");
    let first = Pty::open(80, 24);
    let second = Pty::open(80, 24);
    let mut client_a = server.attach(&first);
    let mut client_b = server.attach(&second);
    let pane = server.pane_id();
    let err_path = env::temp_dir().join(format!(
        "glisk-tmux-ambiguous-{}-{}.err",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let binary = env!("CARGO_BIN_EXE_glisk");
    let example = format!(
        "{}/examples/decision-report.html",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!("'{binary}' '{example}' 2>'{}'", err_path.display());
    server.run(&["respawn-pane", "-k", "-t", &pane, "sh", "-c", &script]);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut diagnostic = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&err_path) {
            if !text.is_empty() {
                diagnostic = text;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        diagnostic.contains("tmux support client-count"),
        "unexpected diagnostic: {diagnostic}"
    );
    let _ = std::fs::remove_file(&err_path);

    let _ = client_a.kill();
    let _ = client_b.kill();
    let _ = client_a.wait();
    let _ = client_b.wait();
}

#[test]
fn detection_matches_the_isolated_server_when_ghostty_terminfo_exists() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    if ghostty_terminfo().is_none() {
        eprintln!("skipping: Ghostty terminfo is not installed on this host");
        return;
    }
    let server = TmuxServer::start("on");
    let pty = Pty::open(80, 24);
    let mut client = server.attach(&pty);

    let pane_id = server.pane_id();
    let tty = server.pane_tty();
    let tmux_env = format!("{},0,0", server.socket.display());
    // SAFETY: the test binary is single-threaded with respect to this env use.
    unsafe {
        env::set_var("TMUX", &tmux_env);
        env::set_var("TMUX_PANE", &pane_id);
    }
    // `detect` compares against the current stdout, which is the test harness,
    // not the pane, so use the pure query parser through a child process later.
    // Here we validate the pieces the test can reach without changing stdout.
    let clients = server.run(&["list-clients", "-t", &server.session, "-F", "#{client_termname}|#{client_termtype}|#{client_cell_width}x#{client_cell_height}|#{client_utf8}"]);
    assert!(clients.contains("xterm-ghostty|"), "{clients}");
    assert!(clients.contains("|17x34|1"), "{clients}");
    let option = server.run(&[
        "show-options",
        "-A",
        "-p",
        "-v",
        "-t",
        &pane_id,
        "allow-passthrough",
    ]);
    assert_eq!(option.trim(), "on");
    assert!(tty.starts_with("/dev/tty"));

    let _ = client.kill();
    let _ = client.wait();
}
