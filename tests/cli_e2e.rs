//! End-to-end CLI tests for the wired Glisk pager.
//!
//! These spawn the real `glisk` binary. Plain-output tests use ordinary
//! pipes. Live tests allocate a PTY, emulate the bounded Kitty/CSI probe, drive
//! keys through the same terminal, and check the cleanup sequence.

#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{FromRawFd, RawFd},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_glisk");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/decision-report.html");
const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;

static PTY_LOCK: Mutex<()> = Mutex::new(());

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command.args(args);
    // These tests emulate the direct-terminal path. Never inherit the tmux
    // pane that happens to be running the test suite.
    command.env_remove("TMUX");
    command.env_remove("TMUX_PANE");
    command
}

fn assert_plain(text: &[u8]) {
    assert!(
        !text.contains(&0x1b),
        "plain output must not contain ESC: {}",
        String::from_utf8_lossy(text)
    );
    assert!(
        !text.windows(2).any(|window| window == b"\x1b["),
        "plain output must not contain terminal controls"
    );
}

#[test]
fn file_text_output_is_plain_and_complete() {
    let output = command(&[FIXTURE, "--text"]).output().expect("run");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(
        output.stderr.is_empty(),
        "diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_plain(&output.stdout);
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        text.starts_with("# Glisk renderer decision report"),
        "{text}"
    );
    assert!(text.contains("## Comparison"), "{text}");
    assert!(text.contains("Evidence 2: known gaps"), "{text}");
    assert!(text.contains("Figure 1."), "{text}");
    assert!(text.ends_with('\n'), "{text}");
}

#[test]
fn animated_example_text_keeps_the_visual_explanation() {
    let output = command(&[
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/animated-cache.html"),
        "--text",
    ])
    .output()
    .expect("run");
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_plain(&output.stdout);
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        text.contains("Top: cache hit. Request → response."),
        "{text}"
    );
    assert!(
        text.contains("Bottom: cache miss. Request → origin → response."),
        "{text}"
    );
}

#[test]
fn stdin_text_output_matches_file_output() {
    let file = command(&[FIXTURE, "--text"]).output().expect("file run");
    assert!(file.status.success());

    let html = std::fs::read(FIXTURE).expect("fixture");
    let mut child = command(&["-", "--text"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&html)
        .expect("write document");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, file.stdout);
    assert_plain(&output.stdout);
}

#[test]
fn redirected_stdout_selects_text_without_graphics() {
    let output = command(&[FIXTURE]).output().expect("run");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(
        output.stderr.is_empty(),
        "diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_plain(&output.stdout);
    assert!(
        output
            .stdout
            .starts_with(b"# Glisk renderer decision report")
    );
    assert!(
        !output.stdout.windows(3).any(|window| window == b"\x1b_"),
        "no Kitty sequence on redirected stdout"
    );
}

#[test]
fn missing_input_file_exits_1() {
    let path = "does-not-exist-glisk-e2e.html";
    let output = command(&[path]).output().expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("glisk:"), "{stderr}");
    assert!(stderr.contains("cannot read document"), "{stderr}");
}

#[test]
fn url_shaped_input_is_rejected() {
    let output = command(&["https://example.invalid/report.html"])
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only local"), "{stderr}");
}

#[test]
fn invalid_utf8_stdin_exits_1() {
    let mut child = command(&["-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&[0xff, 0xfe, 0xfd])
        .expect("write");
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not valid UTF-8"), "{stderr}");
}

#[test]
fn oversized_stdin_exits_1_without_waiting_for_layout() {
    let mut child = command(&["-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let payload = vec![b'x'; MAX_HTML_BYTES + 1];
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let output = child.wait_with_output().expect("wait");
    writer.join().expect("writer");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cap"), "{stderr}");
}

#[test]
fn version_uses_user_facing_name() {
    let output = command(&["--version"]).output().expect("run");
    assert!(output.status.success(), "{:?}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(stdout.starts_with("glisk "), "{stdout}");
}

#[test]
fn scroll_sweep_keeps_viewport_sized_frames() {
    let output = command(&[
        FIXTURE,
        "--scroll-sweep",
        "--css-width",
        "420",
        "--css-height",
        "900",
        "--scale",
        "2",
    ])
    .output()
    .expect("run");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert!(
        stderr.contains("max raster 1512000px"),
        "sweep summary: {stderr}"
    );
    let frames: u32 = stderr
        .split("scroll sweep ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|count| count.parse().ok())
        .unwrap_or_else(|| panic!("frame count: {stderr}"));
    assert!(frames > 1, "multi-screen fixture painted {frames} frame(s)");
}

#[test]
fn broken_stdout_pipe_is_quiet_success() {
    let mut fds = [0; 2];
    // SAFETY: fds has room for two descriptors.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // Close the read end before spawning: every write is a broken pipe.
    // SAFETY: fds[0] was just created by pipe.
    unsafe { libc::close(fds[0]) };
    // SAFETY: fds[1] is owned by the returned File.
    let sink = unsafe { File::from_raw_fd(fds[1]) };

    let child = command(&[FIXTURE, "--text"])
        .stdout(Stdio::from(sink))
        .stderr(Stdio::piped())
        .spawn()
        .expect("run");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "{:?}", output.status);
    assert!(
        output.stderr.is_empty(),
        "broken pipe must be quiet: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// PTY helpers
// ---------------------------------------------------------------------------

struct Pty {
    master: RawFd,
    slave: RawFd,
    _serial: MutexGuard<'static, ()>,
}

impl Pty {
    fn open(cols: u16, rows: u16) -> Self {
        let serial = PTY_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut master = -1;
        let mut slave = -1;
        let mut last_error = 0;
        for _ in 0..20 {
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
            if rc == 0 {
                last_error = 0;
                break;
            }
            last_error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            last_error,
            0,
            "openpty: {}",
            std::io::Error::from_raw_os_error(last_error)
        );
        set_winsize_fd(slave, cols, rows);
        Self {
            master,
            slave,
            _serial: serial,
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: both descriptors came from openpty and are closed once.
        unsafe {
            libc::close(self.master);
            libc::close(self.slave);
        }
    }
}

fn set_winsize_fd(fd: RawFd, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: size is an initialized winsize for this descriptor.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) };
    assert_eq!(rc, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
}

fn dup_fd(fd: RawFd) -> File {
    // SAFETY: dup returns a new owned descriptor.
    let duplicated = unsafe { libc::dup(fd) };
    assert!(duplicated >= 0, "dup: {}", std::io::Error::last_os_error());
    // SAFETY: duplicated is owned by this File.
    unsafe { File::from_raw_fd(duplicated) }
}

fn read_some(fd: RawFd, timeout: Duration) -> Vec<u8> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().clamp(0, i32::MAX as u128) as i32;
    // SAFETY: one initialized pollfd.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc <= 0 {
        return Vec::new();
    }
    let mut buffer = [0u8; 16_384];
    // SAFETY: buffer is writable for its length.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read <= 0 {
        return Vec::new();
    }
    buffer[..read as usize].to_vec()
}

fn write_master(fd: RawFd, bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: bytes pointer and length are valid for this call.
        let written =
            unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        if written <= 0 {
            return;
        }
        offset += written as usize;
    }
}

#[derive(Clone, Copy)]
enum ProbeReply {
    Timeout,
    GraphicsOk,
    GraphicsError,
}

struct Capture {
    data: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Capture {
    fn start(master: RawFd, reply: ProbeReply) -> Self {
        let data = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_data = Arc::clone(&data);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut answered = false;
            while !thread_stop.load(Ordering::SeqCst) {
                let chunk = read_some(master, Duration::from_millis(5));
                if chunk.is_empty() {
                    continue;
                }
                thread_data.lock().unwrap().extend_from_slice(&chunk);
                if answered || matches!(reply, ProbeReply::Timeout) {
                    continue;
                }
                let seen = thread_data.lock().unwrap().clone();
                let saw_cell = seen.windows(5).any(|window| window == b"\x1b[16t");
                let saw_graphics = seen.windows(5).any(|window| window == b"\x1b_Gi=");
                if saw_cell && saw_graphics {
                    let response: &[u8] = match reply {
                        ProbeReply::GraphicsOk => b"\x1b_Gi=31;OK\x1b\\\x1b[6;32;16t",
                        ProbeReply::GraphicsError => b"\x1b_Gi=31;EINVAL\x1b\\",
                        ProbeReply::Timeout => b"",
                    };
                    if !response.is_empty() {
                        write_master(master, response);
                    }
                    answered = true;
                }
            }
        });
        Self {
            data,
            stop,
            handle: Some(handle),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.data.lock().unwrap().clone()
    }

    fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .data
                .lock()
                .unwrap()
                .windows(needle.len())
                .any(|window| window == needle)
            {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn count(&self, needle: &[u8]) -> usize {
        let data = self.data.lock().unwrap();
        data.windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    fn len(&self) -> usize {
        self.data.lock().unwrap().len()
    }

    fn count_after(&self, start: usize, needle: &[u8]) -> usize {
        let data = self.data.lock().unwrap();
        data.get(start..)
            .unwrap_or_default()
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    fn wait_for_after(&self, start: usize, needle: &[u8], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let data = self.data.lock().unwrap();
            if data.len() > start
                && data[start..]
                    .windows(needle.len())
                    .any(|window| window == needle)
            {
                return true;
            }
            drop(data);
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct PtyRun {
    pty: Pty,
    capture: Capture,
    child: Child,
    finished: bool,
}

impl PtyRun {
    fn spawn(args: &[&str], stdin_document: Option<&[u8]>, reply: ProbeReply, term: &str) -> Self {
        let pty = Pty::open(80, 24);

        let (stdin_child, stdin_writer) = match stdin_document {
            Some(document) => {
                let mut fds = [0; 2];
                // SAFETY: fds has room for two descriptors.
                assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
                let document = document.to_vec();
                let writer = thread::spawn(move || {
                    let mut file =
                        // SAFETY: fds[1] is owned by this File.
                        unsafe { File::from_raw_fd(fds[1]) };
                    let _ = file.write_all(&document);
                });
                // SAFETY: fds[0] is owned by this File and moved into stdio.
                (unsafe { File::from_raw_fd(fds[0]) }, Some(writer))
            }
            None => (dup_fd(pty.slave), None),
        };

        let mut command = Command::new(BIN);
        command.args(args);
        command.env("TERM", term);
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
        command.stdin(Stdio::from(stdin_child));
        command.stdout(Stdio::from(dup_fd(pty.slave)));
        command.stderr(Stdio::piped());

        let slave = pty.slave;
        // SAFETY: the child is between fork and exec. setsid detaches from the
        // parent's session and TIOCSCTTY makes the PTY its controlling
        // terminal, so `/dev/tty` is the same terminal as stdout.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(slave, libc::TIOCSCTTY.into(), 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::tcsetpgrp(slave, libc::getpid()) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command.spawn().expect("spawn PTY child");
        if let Some(writer) = stdin_writer {
            writer.join().expect("stdin writer");
        }
        let capture = Capture::start(pty.master, reply);
        Self {
            pty,
            capture,
            child,
            finished: false,
        }
    }

    fn keys(&self, bytes: &[u8]) {
        write_master(self.pty.master, bytes);
    }

    fn resize(&self, cols: u16, rows: u16) {
        set_winsize_fd(self.pty.master, cols, rows);
    }

    fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        self.capture.wait_for(needle, timeout)
    }

    fn wait_for_frames_after(&self, previous: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.capture.count(b"\x1b_Ga=T") > previous {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn graphics_frames(&self) -> usize {
        self.capture.count(b"\x1b_Ga=")
    }

    fn wait_for_graphics_after(&self, previous: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.graphics_frames() > previous {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn wait_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                self.finished = true;
                return status;
            }
            if Instant::now() >= deadline {
                let tail = self.capture.bytes();
                let start = tail.len().saturating_sub(800);
                let _ = self.child.kill();
                let status = self.child.wait().expect("wait after kill");
                let mut stderr = Vec::new();
                if let Some(mut pipe) = self.child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                panic!(
                    "child did not exit: {status:?}; stderr {:?}; captured {} bytes; tail {:?}",
                    String::from_utf8_lossy(&stderr),
                    tail.len(),
                    String::from_utf8_lossy(&tail[start..])
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_end(&mut bytes);
        }
        bytes
    }
}

impl Drop for PtyRun {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn describe_capture(capture: &Capture) -> String {
    let bytes = capture.bytes();
    let tail_start = bytes.len().saturating_sub(600);
    format!(
        "captured {} bytes; tail: {:?}",
        bytes.len(),
        String::from_utf8_lossy(&bytes[tail_start..])
    )
}

struct WatchedReport {
    path: std::path::PathBuf,
}

impl WatchedReport {
    fn new(name: &str, html: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "glisk-watch-{}-{stamp}-{name}.html",
            std::process::id()
        ));
        std::fs::write(&path, html).expect("write watched report");
        Self { path }
    }

    fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn write(&self, html: &str) {
        std::fs::write(&self.path, html).expect("rewrite watched report");
    }

    fn replace_atomically(&self, html: &str) {
        let temporary = self.path.with_extension("replacement");
        std::fs::write(&temporary, html).expect("write replacement");
        std::fs::rename(&temporary, &self.path).expect("atomic replacement");
    }
}

impl Drop for WatchedReport {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("replacement"));
    }
}

fn watch_html(marker: &str) -> String {
    format!(
        "<!doctype html><html><head><style>
         html, body {{ margin: 0; padding: 0; background: #fff; color: #000; }}
         body {{ font: 16px/24px monospace; }}
         section {{ min-height: 240px; }}
         </style></head><body>
         <h1 id=\"watch-top\">Watched report</h1>
         <section id=\"watch-first\"><p>{marker}</p></section>
         <section id=\"watch-last\"><p>Closing section.</p></section>
         </body></html>"
    )
}

#[test]
fn live_pager_follows_save_atomic_replace_and_recreation() {
    let report = WatchedReport::new("follow", &watch_html("alpha revision"));
    let path = report.path();
    let mut run = PtyRun::spawn(&[&path], None, ProbeReply::GraphicsOk, "xterm-ghostty");
    let timeout = Duration::from_secs(6);
    assert!(
        run.wait_for(b"\x1b[?1049h", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    assert!(
        run.wait_for(b"\x1b_Ga=T", timeout),
        "{}",
        describe_capture(&run.capture)
    );

    let first = run.graphics_frames();
    report.write(&watch_html("beta revision"));
    assert!(
        run.wait_for_graphics_after(first, timeout),
        "normal save did not reload: {}",
        describe_capture(&run.capture)
    );

    let replaced = run.graphics_frames();
    report.replace_atomically(&watch_html("gamma revision"));
    assert!(
        run.wait_for_graphics_after(replaced, timeout),
        "atomic replacement did not reload: {}",
        describe_capture(&run.capture)
    );

    let missing = run.graphics_frames();
    std::fs::remove_file(&report.path).expect("remove watched report");
    assert!(
        run.wait_for(b"reload: file missing", timeout),
        "missing replacement did not show status: {}",
        describe_capture(&run.capture)
    );
    assert!(
        run.wait_for_graphics_after(missing, timeout),
        "missing branch must repaint the retained report and status"
    );

    let failed = run.graphics_frames();
    report.write(&watch_html("recovered revision"));
    assert!(
        run.wait_for_graphics_after(failed, timeout),
        "recreation did not recover: {}",
        describe_capture(&run.capture)
    );

    run.keys(b"q");
    assert!(run.wait_exit(timeout).success());
    assert!(run.wait_for(b"\x1b[?1049l", timeout));
    assert!(run.stderr().is_empty());
}

#[test]
fn live_pager_no_watch_holds_the_original_revision() {
    let report = WatchedReport::new("snapshot", &watch_html("snapshot revision"));
    let path = report.path();
    let mut run = PtyRun::spawn(
        &[&path, "--no-watch"],
        None,
        ProbeReply::GraphicsOk,
        "xterm-ghostty",
    );
    let timeout = Duration::from_secs(5);
    assert!(
        run.wait_for(b"\x1b_Ga=T", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    let first = run.graphics_frames();
    report.write(&watch_html("ignored revision"));
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        run.graphics_frames(),
        first,
        "--no-watch must not follow the file: {}",
        describe_capture(&run.capture)
    );

    run.keys(b"q");
    assert!(run.wait_exit(timeout).success());
    assert!(run.stderr().is_empty());
}

#[test]
fn live_pager_handles_navigation_search_resize_and_quit() {
    let mut run = PtyRun::spawn(&[FIXTURE], None, ProbeReply::GraphicsOk, "xterm-ghostty");

    assert!(
        run.wait_for(b"\x1b[?1049h", Duration::from_secs(3)),
        "alternate screen entered"
    );
    assert!(
        run.wait_for(b"\x1b_Ga=T", Duration::from_secs(5)),
        "first frame transmitted"
    );

    // Six Tab presses reach the first disclosure summary; Enter toggles it.
    run.keys(b"\t\t\t\t\t\t\r");
    assert!(
        run.wait_for(b"evidence expanded", Duration::from_secs(3)),
        "focused disclosure opened"
    );

    // Search a phrase that only exists inside the closed second disclosure.
    run.keys(b"/foreground input or resize replies\r");
    assert!(
        run.wait_for(b"1/1", Duration::from_secs(3)),
        "search found and revealed the closed disclosure text"
    );

    let frames = run.capture.count(b"\x1b_Ga=T");
    run.keys(b"n");
    assert!(
        run.wait_for_frames_after(frames, Duration::from_secs(3)),
        "next-match redraw"
    );

    run.resize(100, 30);
    assert!(
        run.wait_for(b",c=100,", Duration::from_secs(3)),
        "resize repainted at the new width"
    );

    let frames = run.capture.count(b"\x1b_Ga=T");
    run.keys(b"q");
    let status = run.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");

    let captured = run.capture.bytes();
    assert!(
        captured
            .windows(b"\x1b_Ga=d".len())
            .any(|window| window == b"\x1b_Ga=d"),
        "image delete and cleanup sequences"
    );
    assert!(
        captured
            .windows(b"\x1b[?1049l".len())
            .any(|window| window == b"\x1b[?1049l"),
        "alternate screen left"
    );
    assert!(
        captured
            .windows(b"\x1b[?25h".len())
            .any(|window| window == b"\x1b[?25h"),
        "cursor restored"
    );
    assert!(frames >= 1);
    let stderr = run.stderr();
    assert!(
        stderr.is_empty(),
        "live run diagnostics: {}",
        String::from_utf8_lossy(&stderr)
    );
}

#[test]
fn live_pager_scrolls_and_activates_controls_with_mouse_input() {
    // The fragment targets the surrounding section, so the reveal does not
    // open the disclosure. The click below therefore starts from the closed
    // state that a reader sees.
    let document = br##"<!doctype html>
        <style>
            body { margin: 0 }
            a, summary, p { display: block; font: 16px/32px sans-serif; margin: 0 }
        </style>
        <div style="height:380px"></div>
        <a href="#panel"><span>Jump to details</span></a>
        <div style="height:388px"></div>
        <section id="panel"><details id="evidence"><summary>Evidence</summary><p>Revealed text</p><a href="https://example.com/report">External link</a></details></section>
        <div style="height:800px"></div>"##;
    let timeout = Duration::from_secs(5);
    let mut run = PtyRun::spawn(
        &["-"],
        Some(document),
        ProbeReply::GraphicsOk,
        "xterm-ghostty",
    );
    assert!(
        run.wait_for(b"glisk | 0%", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    assert!(run.wait_for(b"\x1b[?1006h\x1b[?1000h", timeout));

    // At 40 CSS pixels of scroll, the link covers content row 23. A report
    // split across reads must survive, and the status row (24) must ignore
    // wheel input.
    let frames = run.capture.count(b"\x1b_Ga=");
    run.keys(b"\x1b[<65;2;");
    thread::sleep(Duration::from_millis(120));
    run.keys(b"2M");
    assert!(
        run.wait_for_frames_after(frames, timeout),
        "split wheel report must scroll: {}",
        describe_capture(&run.capture)
    );
    let frames = run.capture.count(b"\x1b_Ga=");
    run.keys(b"\x1b[<65;2;24M");
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        run.capture.count(b"\x1b_Ga="),
        frames,
        "status-row wheel input must be ignored"
    );

    // One press activates once; the release in the same physical click is
    // ignored rather than activating a second time.
    let mark = run.capture.len();
    run.keys(b"\x1b[<0;2;23M\x1b[<0;2;23m");
    assert!(
        run.capture.wait_for_after(mark, b"jumped", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        run.capture.count_after(mark, b"jumped"),
        1,
        "the release must not activate the link again"
    );

    // The fragment reveal aligned the section top. Clicking the summary opens
    // the disclosure, and focus stays on the summary for Enter to close it.
    let mark = run.capture.len();
    run.keys(b"\x1b[<0;2;2M\x1b[<0;2;2m");
    assert!(
        run.capture
            .wait_for_after(mark, b"evidence expanded", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    let mark = run.capture.len();
    run.keys(b"\r");
    assert!(
        run.capture
            .wait_for_after(mark, b"evidence collapsed", timeout),
        "{}",
        describe_capture(&run.capture)
    );

    // A second click reopens from the closed state and activates once.
    let mark = run.capture.len();
    run.keys(b"\x1b[<0;2;2M\x1b[<0;2;2m");
    assert!(
        run.capture
            .wait_for_after(mark, b"evidence expanded", timeout),
        "{}",
        describe_capture(&run.capture)
    );

    // Ordinary content, an external link, and the status row are ignored.
    let frames = run.capture.count(b"\x1b_Ga=");
    run.keys(b"\x1b[<0;2;3M\x1b[<0;2;6M\x1b[<0;2;24M");
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        run.capture.count(b"\x1b_Ga="),
        frames,
        "ineligible clicks must not redraw or activate"
    );

    // Wheel up and wheel down both scroll while the section stays aligned.
    let frames = run.capture.count(b"\x1b_Ga=");
    run.keys(b"\x1b[<64;2;2M");
    assert!(
        run.wait_for_frames_after(frames, timeout),
        "wheel up must scroll"
    );
    let frames = run.capture.count(b"\x1b_Ga=");
    run.keys(b"\x1b[<65;2;2M");
    assert!(
        run.wait_for_frames_after(frames, timeout),
        "wheel down must scroll"
    );

    // Reflow at the new width and map the same control through the new grid.
    run.resize(100, 30);
    assert!(
        run.wait_for(b",c=100,", timeout),
        "resize repainted at the new width"
    );
    let mark = run.capture.len();
    run.keys(b"\x1b[<0;2;2M\x1b[<0;2;2m");
    assert!(
        run.capture
            .wait_for_after(mark, b"evidence collapsed", timeout),
        "click after resize must use the new geometry: {}",
        describe_capture(&run.capture)
    );

    run.keys(b"q");
    assert!(run.wait_exit(timeout).success());
    assert!(run.wait_for(b"\x1b[?1000l\x1b[?1006l", timeout));
    assert!(run.wait_for(b"\x1b[?25h\x1b[?1049l", timeout));
    assert!(run.stderr().is_empty());
}

#[test]
fn live_animated_pager_pauses_and_resumes_without_input_redraws() {
    let document = std::fs::read(format!(
        "{}/examples/animated-cache.html",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("animated example");
    let mut run = PtyRun::spawn(
        &["-", "--pause-animation"],
        Some(&document),
        ProbeReply::GraphicsOk,
        "xterm-ghostty",
    );
    let timeout = Duration::from_secs(5);
    assert!(
        run.wait_for(b"paused (p)", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    let paused_frames = run.capture.count(b"\x1b_Ga=T");
    thread::sleep(Duration::from_millis(350));
    assert_eq!(run.capture.count(b"\x1b_Ga=T"), paused_frames);

    run.keys(b"p");
    assert!(
        run.wait_for(b"playing (p)", timeout),
        "{}",
        describe_capture(&run.capture)
    );
    assert!(
        run.wait_for_frames_after(paused_frames + 1, timeout),
        "animation should redraw without further input"
    );

    run.keys(b"p");
    assert!(run.wait_for(b"paused (p)", timeout));
    let resumed_frames = run.capture.count(b"\x1b_Ga=T");
    thread::sleep(Duration::from_millis(250));
    assert_eq!(run.capture.count(b"\x1b_Ga=T"), resumed_frames);
    run.keys(b"q");
    assert!(run.wait_exit(timeout).success());
}

#[test]
fn stdin_document_uses_pty_keyboard_and_quits() {
    let document = std::fs::read(FIXTURE).expect("fixture");
    let mut run = PtyRun::spawn(
        &["-"],
        Some(&document),
        ProbeReply::GraphicsOk,
        "xterm-ghostty",
    );

    if !run.wait_for(b"\x1b_Ga=T", Duration::from_secs(5)) {
        let status = run.wait_exit(Duration::from_secs(3));
        let stderr = String::from_utf8_lossy(&run.stderr()).into_owned();
        panic!(
            "stdin document did not paint: {status:?}; stderr {stderr}; {}",
            describe_capture(&run.capture)
        );
    }
    assert!(
        run.wait_for(b"\x1b[?1049h", Duration::from_secs(3)),
        "live mode entered with piped stdin"
    );

    run.keys(b"q");
    let status = run.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");
    let captured = run.capture.bytes();
    assert!(
        captured
            .windows(b"\x1b[?1049l".len())
            .any(|window| window == b"\x1b[?1049l"),
        "alternate screen left"
    );
}

#[test]
fn probe_timeout_falls_back_to_text_on_the_tty() {
    let mut run = PtyRun::spawn(&[FIXTURE], None, ProbeReply::Timeout, "xterm-256color");
    let status = run.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");

    let captured = run.capture.bytes();
    assert!(
        !captured.windows(5).any(|window| window == b"a=T,f"),
        "timeout must not transmit a graphics frame"
    );
    assert!(
        captured
            .windows(b"# Glisk renderer decision report".len())
            .any(|window| window == b"# Glisk renderer decision report"),
        "plain text was written to the terminal"
    );
    let stderr = String::from_utf8_lossy(&run.stderr()).into_owned();
    assert!(stderr.contains("graphics unavailable"), "{stderr}");
    assert!(stderr.contains("timeout"), "{stderr}");
}

#[test]
fn unknown_terminal_graphics_rejection_falls_back_to_text() {
    let mut run = PtyRun::spawn(
        &[FIXTURE],
        None,
        ProbeReply::GraphicsError,
        "unknown-terminal",
    );
    let status = run.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");

    let captured = run.capture.bytes();
    assert!(
        !captured.windows(5).any(|window| window == b"a=T,f"),
        "rejected graphics must not transmit a frame"
    );
    assert!(
        captured
            .windows(b"# Glisk renderer decision report".len())
            .any(|window| window == b"# Glisk renderer decision report"),
        "plain text was written to the terminal"
    );
    let stderr = String::from_utf8_lossy(&run.stderr()).into_owned();
    assert!(stderr.contains("graphics unavailable"), "{stderr}");
    assert!(stderr.contains("unsupported-graphics"), "{stderr}");
}
