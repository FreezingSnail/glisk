//! Deterministic pseudo-terminal tests for the terminal owner
//! (`src/terminal.rs`, bead `glisk-2hp.3`).
//!
//! The module is included by path because wiring into `main.rs` belongs to
//! bead `glisk-2hp.8`. Every test that opens a PTY keeps the master side as a
//! tiny terminal emulator: it answers the bounded probes, captures the
//! escape-sequence stream, and checks mode restoration.

#[path = "../src/tmux.rs"]
mod tmux;

#[path = "../src/terminal.rs"]
mod terminal;

use std::{
    os::fd::RawFd,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use terminal::{
    CellPixels, FrameStats, Geometry, InputBatch, ProbeFailureKind, TerminalConfig, TerminalError,
    TerminalOwner,
};

static PTY_LOCK: Mutex<()> = Mutex::new(());

struct Pty {
    master: RawFd,
    slave: RawFd,
    _serial: MutexGuard<'static, ()>,
}

impl Pty {
    fn open(cols: u16, rows: u16) -> Self {
        // Keep PTY allocation serial inside this test binary. Parallel test
        // threads can exhaust macOS PTY slots; retry briefly for another
        // process on the host doing the same.
        let serial = PTY_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut master = -1;
        let mut slave = -1;
        let mut last_error = 0;
        for _ in 0..20 {
            // SAFETY: openpty writes the two descriptors on success.
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
            "openpty: {} (os error {last_error})",
            std::io::Error::from_raw_os_error(last_error)
        );
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        set_winsize_fd(slave, &size);
        Self {
            master,
            slave,
            _serial: serial,
        }
    }
}

fn set_winsize_fd(fd: RawFd, size: &libc::winsize) {
    // SAFETY: size is an initialized winsize for this descriptor.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, size) };
    assert_eq!(rc, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
}

fn set_winsize(fd: RawFd, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    set_winsize_fd(fd, &size);
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

fn read_some(fd: RawFd, timeout: Duration) -> Vec<u8> {
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
    // SAFETY: one initialized pollfd.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc <= 0 {
        return Vec::new();
    }
    let mut buffer = [0u8; 8192];
    // SAFETY: buffer is writable for its full length.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read <= 0 {
        return Vec::new();
    }
    buffer[..read as usize].to_vec()
}

fn drain(fd: RawFd) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let chunk = read_some(fd, Duration::from_millis(30));
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    out
}

fn write_master(fd: RawFd, bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        // SAFETY: bytes pointer and length are valid for this call.
        let written =
            unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        assert!(written > 0, "write to pty master");
        offset += written as usize;
    }
}

/// Present while another thread drains the master side. PTY output buffers are
/// smaller than a multi-chunk Kitty frame, so a synchronous write would block.
fn present_captured(owner: &mut TerminalOwner, master: RawFd, png: &[u8]) -> (FrameStats, Vec<u8>) {
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let reader = thread::spawn(move || {
        let mut captured = Vec::new();
        while !reader_stop.load(Ordering::SeqCst) {
            captured.extend(read_some(master, Duration::from_millis(2)));
        }
        loop {
            let chunk = read_some(master, Duration::from_millis(10));
            if chunk.is_empty() {
                break;
            }
            captured.extend(chunk);
        }
        captured
    });
    let stats = owner.present(png).expect("present");
    stop.store(true, Ordering::SeqCst);
    let wire = reader.join().expect("drainer");
    (stats, wire)
}

#[derive(Clone, Copy)]
enum ProbePlan {
    Success,
    Timeout,
    GraphicsOnly,
    GraphicsError,
    MalformedCell,
    IncompleteCell,
}

fn spawn_reply(master: RawFd, reply: Vec<u8>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(800);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(read_some(master, Duration::from_millis(10)));
            if seen.windows(5).any(|window| window == b"\x1b[16t") {
                break;
            }
        }
        write_master(master, &reply);
    })
}

fn spawn_probe_service(master: RawFd, plan: ProbePlan) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(800);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(read_some(master, Duration::from_millis(10)));
            let saw_cell = seen.windows(5).any(|window| window == b"\x1b[16t");
            let saw_graphics = seen.windows(3).any(|window| window == b"\x1b_G");
            if saw_cell && saw_graphics {
                break;
            }
        }

        let mut reply = Vec::new();
        match plan {
            ProbePlan::Success => {
                // Ordinary key typed while the probe is in flight.
                reply.extend_from_slice(b"j");
                reply.extend_from_slice(b"\x1b_Gi=31;OK\x1b\\");
                reply.extend_from_slice(b"\x1b[6;32;16t");
            }
            ProbePlan::Timeout => {}
            ProbePlan::GraphicsOnly => {
                reply.extend_from_slice(b"\x1b_Gi=31;OK\x1b\\");
            }
            ProbePlan::GraphicsError => {
                reply.extend_from_slice(b"\x1b_Gi=31;EINVAL\x1b\\");
            }
            ProbePlan::MalformedCell => {
                reply.extend_from_slice(b"\x1b_Gi=31;OK\x1b\\");
                reply.extend_from_slice(b"\x1b[6;32t");
            }
            ProbePlan::IncompleteCell => {
                reply.extend_from_slice(b"\x1b_Gi=31;OK\x1b\\");
                reply.extend_from_slice(b"\x1b[6;32;");
            }
        }
        if !reply.is_empty() {
            write_master(master, &reply);
        }
    })
}

fn get_termios(fd: RawFd) -> libc::termios {
    // SAFETY: termios is zeroed and filled by tcgetattr.
    unsafe {
        let mut term: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(fd, &mut term), 0, "tcgetattr");
        term
    }
}

fn assert_modes_restored(before: &libc::termios, after: &libc::termios) {
    // macOS reports PENDIN transiently after pending input; it is not a saved
    // mode and cannot be restored by tcsetattr.
    const PENDIN: libc::tcflag_t = 0x2000_0000;
    assert_eq!(before.c_iflag, after.c_iflag, "input modes restored");
    assert_eq!(before.c_oflag, after.c_oflag, "output modes restored");
    assert_eq!(
        before.c_lflag & !PENDIN,
        after.c_lflag & !PENDIN,
        "local modes restored"
    );
}

fn probe_config(budget_ms: u64) -> TerminalConfig {
    TerminalConfig::default().with_probe_budget(Duration::from_millis(budget_ms))
}

fn open_with_plan(
    pty: &Pty,
    config: TerminalConfig,
    plan: ProbePlan,
) -> Result<TerminalOwner, TerminalError> {
    let service = spawn_probe_service(pty.master, plan);
    let result = TerminalOwner::from_fds(pty.slave, pty.slave, config);
    service.join().expect("probe service");
    result
}

fn probe_failure(err: TerminalError) -> ProbeFailureKind {
    match err {
        TerminalError::Probe(err) => err.kind,
        other => panic!("expected probe failure, got {other:?}"),
    }
}

/// A syntactically valid PNG header plus filler. The terminal owner only
/// validates the signature and bounds; rasterization belongs to the renderer.
fn fake_png(len: usize, marker: u8) -> Vec<u8> {
    assert!(len >= 24);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.resize(len, marker);
    png[12..16].copy_from_slice(b"IHDR");
    png
}

fn fake_png_with_raster(width: u32, height: u32, len: usize, marker: u8) -> Vec<u8> {
    let mut png = fake_png(len, marker);
    png[16..20].copy_from_slice(&width.to_be_bytes());
    png[20..24].copy_from_slice(&height.to_be_bytes());
    png
}

/// Payload of every APC sequence in the captured stream, in order.
fn kitty_payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'_' && bytes[i + 2] == b'G' {
            let start = i + 3;
            match bytes[start..]
                .windows(2)
                .position(|window| window == b"\x1b\\")
            {
                Some(offset) => {
                    payloads.push(bytes[start..start + offset].to_vec());
                    i = start + offset + 2;
                }
                None => break,
            }
        } else {
            i += 1;
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn probe_success_reports_geometry_and_retains_keys() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let config = probe_config(400);
    let mut owner = open_with_plan(&pty, config, ProbePlan::Success).expect("probe succeeds");

    let geometry = owner.geometry();
    assert_eq!(geometry.cols(), 80);
    assert_eq!(geometry.rows(), 24);
    assert_eq!(geometry.content_rows(), 23);
    assert_eq!(geometry.cell(), CellPixels::new(16, 32).unwrap());
    assert_eq!(geometry.cell().width(), 16);
    assert_eq!(geometry.cell().height(), 32);
    assert_eq!(geometry.status_rows(), 1);
    assert_eq!(geometry.physical_size(), (1280, 736));
    assert_eq!(geometry.css_size(), (640.0, 368.0));
    assert_eq!(geometry.scale(), 2.0);

    let batch = owner
        .poll_input(Duration::from_millis(0))
        .expect("poll")
        .expect("probe-time key retained");
    assert!(!batch.is_empty());
    assert_eq!(
        batch,
        InputBatch {
            bytes: b"j".to_vec(),
            interrupt: false,
        }
    );
    assert_eq!(owner.probe_report().cell, CellPixels::new(16, 32).unwrap());
    let _environment = owner.environment();

    owner.finish().expect("finish");
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn reprobe_cell_measures_again_and_retains_keys() {
    let pty = Pty::open(80, 24);
    let mut owner = open_with_plan(&pty, probe_config(400), ProbePlan::Success).expect("probe");
    let _ = owner
        .poll_input(Duration::from_millis(0))
        .expect("probe key");
    let _ = drain(pty.master);

    let service = spawn_reply(pty.master, b"k\x1b[6;53;26t".to_vec());
    let cell = owner.reprobe_cell().expect("cell reprobe");
    service.join().expect("reprobe service");
    assert_eq!(cell, CellPixels::new(26, 53).unwrap());

    let batch = owner
        .poll_input(Duration::from_millis(0))
        .expect("poll")
        .expect("key retained during reprobe");
    assert_eq!(batch.bytes, b"k".to_vec());

    let cols = owner.geometry().cols();
    let rows = owner.geometry().rows();
    let geometry = owner.resize_with_cell(cols, rows, cell).expect("new cell");
    assert_eq!(geometry.cell(), CellPixels::new(26, 53).unwrap());
    owner.finish().expect("finish");
}

#[test]
fn probe_timeout_is_deterministic_and_restores_modes() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let started = Instant::now();
    let err = open_with_plan(&pty, probe_config(70), ProbePlan::Timeout).expect_err("timeout");
    assert_eq!(probe_failure(err), ProbeFailureKind::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn probe_unexpected_reply_is_rejected() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let err =
        open_with_plan(&pty, probe_config(300), ProbePlan::MalformedCell).expect_err("unexpected");
    assert_eq!(probe_failure(err), ProbeFailureKind::UnexpectedReply);
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn probe_incomplete_reply_is_rejected() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let err =
        open_with_plan(&pty, probe_config(70), ProbePlan::IncompleteCell).expect_err("incomplete");
    assert_eq!(probe_failure(err), ProbeFailureKind::IncompleteReply);
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn probe_missing_geometry_is_rejected() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let err =
        open_with_plan(&pty, probe_config(70), ProbePlan::GraphicsOnly).expect_err("no geometry");
    assert_eq!(probe_failure(err), ProbeFailureKind::MissingGeometry);
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn probe_explicit_graphics_negative_is_rejected() {
    let pty = Pty::open(80, 24);
    let before = get_termios(pty.slave);
    let err =
        open_with_plan(&pty, probe_config(300), ProbePlan::GraphicsError).expect_err("unsupported");
    assert_eq!(probe_failure(err), ProbeFailureKind::UnsupportedGraphics);
    assert_modes_restored(&before, &get_termios(pty.slave));
}

#[test]
fn frame_uses_one_image_chunks_and_preserves_cursor() {
    let pty = Pty::open(80, 24);
    let config = probe_config(400).with_chunk_bytes(4000);
    let mut owner = open_with_plan(&pty, config, ProbePlan::Success).expect("probe");
    let image_id = owner.image_id();
    let _ = drain(pty.master);

    assert!(matches!(
        owner.present(b"not a png"),
        Err(TerminalError::InvalidPng)
    ));

    let png = fake_png(9000, 7);
    let (stats, wire) = present_captured(&mut owner, pty.master, &png);
    let encoded = base64_encode(&png);
    assert_eq!(stats.png_bytes, 9000);
    assert_eq!(stats.encoded_bytes, encoded.len());
    assert_eq!(stats.chunks, encoded.len().div_ceil(4000));
    assert!(contains(&wire, b"\x1b7\x1b[H\x1b_Ga=T,f=100,i="));
    assert!(contains(&wire, b",q=2,C=1,c=80,r=23,m=1;"));
    assert_eq!(count(&wire, b"\x1b_Gm=1;"), stats.chunks - 2);
    assert_eq!(count(&wire, b"\x1b_Gm=0;"), 1);
    let last_chunk = wire
        .windows(2)
        .rposition(|window| window == b"\x1b\\")
        .expect("last chunk terminator");
    let tail = &wire[last_chunk + 2..];
    assert!(
        contains(tail, b"\x1b8"),
        "frame restores cursor after last chunk"
    );

    let payloads = kitty_payloads(&wire);
    assert_eq!(payloads.len(), stats.chunks);
    let mut joined = Vec::new();
    for (index, payload) in payloads.iter().enumerate() {
        let data = payload_data(payload);
        assert!(data.len() <= 4000);
        if index > 0 {
            assert!(payload.starts_with(b"m="));
        }
        joined.extend_from_slice(data);
    }
    assert_eq!(joined, encoded);

    assert!(contains(&wire, format!("i={image_id},").as_bytes()));
    owner.finish().expect("finish");
}

#[test]
fn multiple_frames_replace_one_image_id() {
    let pty = Pty::open(50, 20);
    let mut owner = open_with_plan(
        &pty,
        probe_config(400).with_frame_editing(false),
        ProbePlan::Success,
    )
    .expect("probe");
    let image_id = owner.image_id();
    let _ = drain(pty.master);

    let first = fake_png(300, 1);
    let second = fake_png(700, 2);
    let (_first_stats, first_wire) = present_captured(&mut owner, pty.master, &first);
    let (_second_stats, second_wire) = present_captured(&mut owner, pty.master, &second);

    let header = format!("a=T,f=100,i={image_id},");
    assert!(contains(&first_wire, header.as_bytes()));
    assert!(contains(&second_wire, header.as_bytes()));
    assert_eq!(count(&first_wire, header.as_bytes()), 1);
    assert_eq!(count(&second_wire, header.as_bytes()), 1);
    assert_eq!(
        count(&second_wire, b"a=d,d=I,"),
        0,
        "no delete between frames"
    );

    let first_payload: Vec<u8> = kitty_payloads(&first_wire)
        .iter()
        .flat_map(|payload| payload_data(payload).to_vec())
        .collect();
    let second_payload: Vec<u8> = kitty_payloads(&second_wire)
        .iter()
        .flat_map(|payload| payload_data(payload).to_vec())
        .collect();
    assert_eq!(first_payload, base64_encode(&first));
    assert_eq!(second_payload, base64_encode(&second));
    assert_ne!(first_payload, second_payload);

    owner.finish().expect("finish");
    let cleanup = drain(pty.master);
    assert_eq!(
        count(
            &cleanup,
            format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").as_bytes()
        ),
        1
    );
}

#[test]
fn steady_frame_edits_overwrite_base_frame_without_replacing_placement() {
    let pty = Pty::open(80, 24);
    let mut owner = open_with_plan(
        &pty,
        probe_config(400).with_frame_editing(true),
        ProbePlan::Success,
    )
    .expect("probe");
    assert!(owner.frame_editing_supported());
    let image_id = owner.image_id();
    let _ = drain(pty.master);

    let first = fake_png_with_raster(1280, 736, 9000, 1);
    let (_, first_wire) = present_captured(&mut owner, pty.master, &first);
    assert!(contains(&first_wire, b"a=T,f=100"));
    assert!(contains(&first_wire, b",C=1,c=80,r=23,"));

    let second = fake_png_with_raster(1280, 736, 9000, 2);
    let (second_stats, second_wire) = present_captured(&mut owner, pty.master, &second);
    assert!(!contains(&second_wire, b"a=T"));
    assert!(!contains(&second_wire, b"a=d"));
    assert!(!contains(&second_wire, b"p=1"));
    assert!(!contains(&second_wire, b"U=1"));
    assert!(!contains(&second_wire, b"C=1"));
    assert!(!contains(&second_wire, b",c=80,r=23"));
    assert!(contains(
        &second_wire,
        format!("\x1b_Ga=f,f=100,i={image_id},r=1,X=1,q=2,m=1;").as_bytes()
    ));
    assert_eq!(count(&second_wire, b"\x1b_Ga=f,m=1;"), second_stats.chunks - 2);
    assert_eq!(count(&second_wire, b"\x1b_Ga=f,m=0;"), 1);

    let payload: Vec<u8> = kitty_payloads(&second_wire)
        .iter()
        .flat_map(|payload| payload_data(payload).to_vec())
        .collect();
    let decoded = base64_decode(&payload);
    assert!(decoded.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(decoded, second);

    // A changed raster at the same cell geometry must re-place the image.
    let resized_raster = fake_png_with_raster(1200, 700, 9000, 3);
    let (_, raster_wire) = present_captured(&mut owner, pty.master, &resized_raster);
    assert!(contains(&raster_wire, b"a=T,f=100"));
    assert!(contains(&raster_wire, b",C=1,c=80,r=23,"));

    let same_raster = fake_png_with_raster(1200, 700, 9000, 4);
    let (_, edit_wire) = present_captured(&mut owner, pty.master, &same_raster);
    assert!(!contains(&edit_wire, b"a=T"));
    assert!(contains(&edit_wire, b"a=f,f=100"));

    // A grid resize changes the placement and must bootstrap again.
    owner.resize(40, 12).expect("resize");
    let (_, resized_wire) = present_captured(
        &mut owner,
        pty.master,
        &fake_png_with_raster(640, 352, 4000, 5),
    );
    assert!(contains(&resized_wire, b"a=T,f=100"));
    assert!(contains(&resized_wire, b",C=1,c=40,r=11,"));

    owner.finish().expect("finish");
}

#[test]
fn status_row_uses_last_row_and_strips_controls() {
    let pty = Pty::open(80, 24);
    let mut owner = open_with_plan(&pty, probe_config(400), ProbePlan::Success).expect("probe");
    let _ = drain(pty.master);

    owner.paint_status("hello\x1b[31mworld\n").expect("status");
    let wire = drain(pty.master);
    assert!(contains(&wire, b"\x1b7\x1b[24;1H\x1b[2Khelloworld\x1b8"));
    assert!(!contains(&wire, b"\x1b[31m"), "document controls stripped");

    owner.paint_status("abcdefghij").expect("status truncate");
    let wire = drain(pty.master);
    assert!(contains(&wire, "abcdefghij".as_bytes()));
    owner
        .paint_status(&"x".repeat(100))
        .expect("status truncate long");
    let wire = drain(pty.master);
    let expected = format!("{}…", "x".repeat(79));
    assert!(contains(&wire, expected.as_bytes()));
    owner.finish().expect("finish");
}

#[test]
fn resize_recomputes_and_rejects_impossible_geometry() {
    let pty = Pty::open(80, 24);
    let mut owner = open_with_plan(&pty, probe_config(400), ProbePlan::Success).expect("probe");
    let _ = drain(pty.master);

    let geometry = owner.resize(40, 12).expect("resize");
    assert_eq!(geometry.cols(), 40);
    assert_eq!(geometry.rows(), 12);
    assert_eq!(geometry.content_rows(), 11);
    assert_eq!(geometry.physical_size(), (640, 352));
    assert_eq!(geometry.css_size(), (320.0, 176.0));

    let too_small = owner.resize(40, 1);
    assert!(matches!(too_small, Err(TerminalError::Geometry(_))));
    assert_eq!(owner.geometry().rows(), 12, "failed resize rolls back");

    set_winsize(pty.slave, 60, 18);
    let refreshed = owner
        .refresh_size()
        .expect("refresh")
        .expect("grid changed");
    assert_eq!(refreshed.cols(), 60);
    assert_eq!(refreshed.rows(), 18);
    assert!(owner.refresh_size().expect("refresh").is_none());
    owner.resize(40, 12).expect("resize back");

    owner
        .present(&fake_png(64, 3))
        .expect("present after resize");
    let wire = drain(pty.master);
    assert!(contains(&wire, b",q=2,C=1,c=40,r=11,m=0;"));
    owner.finish().expect("finish");
}

#[test]
fn cleanup_runs_after_interrupt_and_handled_error() {
    let pty = Pty::open(60, 18);
    let before = get_termios(pty.slave);
    let mut owner = open_with_plan(&pty, probe_config(400), ProbePlan::Success).expect("probe");
    let image_id = owner.image_id();
    let _ = owner
        .poll_input(Duration::from_millis(0))
        .expect("probe key");
    let _ = drain(pty.master);

    owner.present(&fake_png(200, 4)).expect("present");
    let _ = drain(pty.master);

    write_master(pty.master, b"\x03");
    let batch = owner
        .poll_input(Duration::from_millis(300))
        .expect("poll")
        .expect("interrupt");
    assert!(batch.interrupt);
    assert!(batch.bytes.is_empty());

    owner.finish().expect("finish after interrupt");
    let cleanup = drain(pty.master);
    assert!(contains(
        &cleanup,
        format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").as_bytes()
    ));
    assert!(contains(&cleanup, b"\x1b[?25h"));
    assert!(contains(&cleanup, b"\x1b[?1049l"));
    assert_modes_restored(&before, &get_termios(pty.slave));
    drop(owner);
    drop(pty);

    // Handled error path: a frame above the configured bound fails before any
    // Kitty write, and the same owner still cleans up.
    let pty = Pty::open(60, 18);
    let before = get_termios(pty.slave);
    let config = probe_config(400).with_png_limit(32);
    let mut owner = open_with_plan(&pty, config, ProbePlan::Success).expect("probe");
    let _ = owner
        .poll_input(Duration::from_millis(0))
        .expect("probe key");
    let _ = drain(pty.master);
    let err = owner.present(&fake_png(64, 5)).expect_err("too large");
    assert!(matches!(err, TerminalError::FrameTooLarge { .. }));
    assert_eq!(count(&drain(pty.master), b"a=T"), 0);
    owner.finish().expect("finish after error");
    let cleanup = drain(pty.master);
    assert!(contains(&cleanup, b"\x1b[?25h"));
    assert!(contains(&cleanup, b"\x1b[?1049l"));
    assert_modes_restored(&before, &get_termios(pty.slave));
    assert!(matches!(
        owner.paint_status("late"),
        Err(TerminalError::Finished)
    ));
}

#[test]
fn tty_identity_checks_reject_crossed_or_non_tty_descriptors() {
    let pty = Pty::open(80, 24);
    assert!(terminal::is_tty(pty.slave));
    assert!(terminal::same_tty(pty.slave, pty.slave));
    assert!(!terminal::same_tty(pty.master, pty.slave));

    let crossed = TerminalOwner::from_fds(pty.master, pty.slave, TerminalConfig::default())
        .expect_err("different devices");
    assert!(matches!(crossed, TerminalError::NotSameTerminal));

    let mut fds = [0; 2];
    // SAFETY: fds has room for two descriptors.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let piped =
        TerminalOwner::from_fds(fds[0], fds[1], TerminalConfig::default()).expect_err("not a tty");
    assert!(matches!(piped, TerminalError::NotATerminal("input")));
    // SAFETY: fds were opened by pipe.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

#[test]
fn geometry_rejects_oversized_raster_before_allocation() {
    let cell = CellPixels::new(4096, 4096).unwrap();
    let err = Geometry::compute(4096, 4096, cell, 0).expect_err("status rows");
    assert!(err.to_string().contains("status row"));
    let err = Geometry::compute(2000, 2000, cell, 1).expect_err("raster budget");
    assert!(err.to_string().contains("exceeds"));
}

fn base64_encode(bytes: &[u8]) -> Vec<u8> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.encode(bytes).into_bytes()
}

fn base64_decode(bytes: &[u8]) -> Vec<u8> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.decode(bytes).expect("base64 payload")
}
