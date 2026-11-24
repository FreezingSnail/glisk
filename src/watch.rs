//! Bounded local-file revision watching for the live pager.
//!
//! Bead: `glisk-2hp.12`. The watcher observes exactly one path and reports a
//! revision only after its metadata signature has stayed unchanged for a short
//! debounce window. It never opens the file, never follows a directory, and
//! never starts a timer: the pager calls [`Watcher::poll`] from its existing
//! input loop.
//!
//! A signature covers size, modification/change metadata, and the device/inode
//! pair on Unix, so in-place writes and atomic rename replacement both look
//! like a new revision. A missing file, an unreadable path, and a non-regular
//! replacement are distinct states; none of them blocks or performs document
//! work. The pager keeps the last good report and can recover when the path
//! becomes a regular file again.

use std::{
    fs::Metadata,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// Minimum interval between path metadata checks.
pub const CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// A candidate signature must remain stable for this long before ingestion.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Identity of the watched path at one metadata check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathState {
    /// No directory entry (including removal before recreation).
    Missing,
    /// The entry exists but is not a regular file (directory, FIFO, socket).
    NotRegular,
    /// Metadata could not be read (for example, a permission failure).
    Unreadable,
    /// A regular file with the metadata signature below.
    File(FileSignature),
}

/// Comparable metadata for one regular file revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSignature {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mtime: i64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl FileSignature {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: {
                use std::os::unix::fs::MetadataExt;
                metadata.dev()
            },
            #[cfg(unix)]
            ino: {
                use std::os::unix::fs::MetadataExt;
                metadata.ino()
            },
            #[cfg(unix)]
            mtime: {
                use std::os::unix::fs::MetadataExt;
                metadata.mtime()
            },
            #[cfg(unix)]
            mtime_nsec: {
                use std::os::unix::fs::MetadataExt;
                metadata.mtime_nsec()
            },
            #[cfg(unix)]
            ctime: {
                use std::os::unix::fs::MetadataExt;
                metadata.ctime()
            },
            #[cfg(unix)]
            ctime_nsec: {
                use std::os::unix::fs::MetadataExt;
                metadata.ctime_nsec()
            },
        }
    }
}

/// Metadata-only state of one path. Never blocks on file contents.
pub fn path_state(path: &Path) -> PathState {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            PathState::File(FileSignature::from_metadata(&metadata))
        }
        Ok(_) => PathState::NotRegular,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PathState::Missing,
        Err(_) => PathState::Unreadable,
    }
}

/// Stable content digest used to skip a metadata change that did not alter bytes.
pub fn content_digest(bytes: &[u8]) -> u64 {
    // FNV-1a 64: tiny, bounded, and sufficient to skip unchanged revisions.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Initial state captured before the first ingestion.
#[derive(Clone, Debug)]
pub struct Baseline {
    pub state: PathState,
    pub digest: u64,
}

/// One path watcher for one live pager session.
#[derive(Debug)]
pub struct Watcher {
    path: PathBuf,
    known: PathState,
    digest: u64,
    candidate: Option<(PathState, Instant)>,
    failed: Option<PathState>,
    next_check: Instant,
}

impl Watcher {
    pub fn new(path: impl Into<PathBuf>, baseline: Baseline) -> Self {
        Self {
            path: path.into(),
            known: baseline.state,
            digest: baseline.digest,
            candidate: None,
            failed: None,
            next_check: Instant::now(),
        }
    }

    /// Digest of the source matching the currently displayed document.
    pub fn known_digest(&self) -> u64 {
        self.digest
    }

    /// Throttled check. Returns a stable candidate revision whose signature
    /// differs from both the accepted revision and the last rejected one.
    pub fn poll(&mut self, now: Instant) -> Option<PathState> {
        if now < self.next_check {
            return None;
        }
        self.next_check = now + CHECK_INTERVAL;
        let state = path_state(&self.path);
        self.poll_state(state, now)
    }

    /// Debounce one already-read state. Exposed for deterministic state tests.
    fn poll_state(&mut self, state: PathState, now: Instant) -> Option<PathState> {
        if state == self.known || self.failed.as_ref() == Some(&state) {
            self.candidate = None;
            return None;
        }
        match &self.candidate {
            Some((candidate, since)) if candidate == &state => {
                if now.duration_since(*since) >= DEBOUNCE {
                    self.candidate = None;
                    Some(state)
                } else {
                    None
                }
            }
            _ => {
                self.candidate = Some((state, now));
                None
            }
        }
    }

    /// Accept a revision whose source was opened successfully.
    pub fn accept(&mut self, state: PathState, digest: u64) {
        self.known = state;
        self.digest = digest;
        self.candidate = None;
        self.failed = None;
    }

    /// Remember one failed revision so the same state is not retried forever.
    pub fn reject(&mut self, state: PathState) {
        self.failed = Some(state);
        self.candidate = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn sample(seed: u64) -> PathState {
        PathState::File(FileSignature {
            len: seed,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seed)),
            dev: 1,
            ino: seed + 10,
            mtime: seed as i64,
            mtime_nsec: 0,
            ctime: seed as i64,
            ctime_nsec: 0,
        })
    }

    #[cfg(not(unix))]
    fn sample(seed: u64) -> PathState {
        PathState::File(FileSignature {
            len: seed,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seed)),
        })
    }

    fn watcher() -> Watcher {
        Watcher::new(
            "unused.html",
            Baseline {
                state: sample(1),
                digest: 7,
            },
        )
    }

    #[test]
    fn stable_change_is_reported_once_after_the_debounce_window() {
        let mut watcher = watcher();
        let start = Instant::now();
        assert!(watcher.poll_state(sample(2), start).is_none());
        assert!(
            watcher
                .poll_state(sample(2), start + Duration::from_millis(50))
                .is_none()
        );
        assert_eq!(
            watcher.poll_state(sample(2), start + DEBOUNCE),
            Some(sample(2))
        );
        assert!(watcher.poll_state(sample(2), start + DEBOUNCE).is_none());
    }

    #[test]
    fn a_burst_restarts_the_debounce_window() {
        let mut watcher = watcher();
        let start = Instant::now();
        assert!(watcher.poll_state(sample(2), start).is_none());
        assert!(
            watcher
                .poll_state(sample(3), start + Duration::from_millis(120))
                .is_none()
        );
        assert!(
            watcher
                .poll_state(sample(3), start + Duration::from_millis(180))
                .is_none()
        );
        assert_eq!(
            watcher.poll_state(sample(3), start + Duration::from_millis(280)),
            Some(sample(3))
        );
    }

    #[test]
    fn unchanged_and_known_revisions_are_ignored() {
        let mut watcher = watcher();
        let start = Instant::now();
        assert!(watcher.poll_state(sample(1), start).is_none());
        assert!(
            watcher
                .poll_state(sample(1), start + Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn a_rejected_revision_is_not_retried_until_it_changes() {
        let mut watcher = watcher();
        let start = Instant::now();
        assert!(watcher.poll_state(PathState::Missing, start).is_none());
        assert_eq!(
            watcher.poll_state(PathState::Missing, start + DEBOUNCE),
            Some(PathState::Missing)
        );
        watcher.reject(PathState::Missing);
        assert!(
            watcher
                .poll_state(PathState::Missing, start + Duration::from_secs(2))
                .is_none()
        );
        watcher.accept(sample(4), 9);
        let later = start + Duration::from_secs(3);
        assert!(watcher.poll_state(PathState::Missing, later).is_none());
        assert_eq!(
            watcher.poll_state(PathState::Missing, later + DEBOUNCE),
            Some(PathState::Missing)
        );
        assert_eq!(watcher.known_digest(), 9);
    }

    #[test]
    fn content_digest_separates_changed_bytes() {
        assert_eq!(content_digest(b"same"), content_digest(b"same"));
        assert_ne!(content_digest(b"same"), content_digest(b"other"));
    }

    #[test]
    fn paths_report_missing_regular_and_non_regular_states() {
        let root = std::env::current_dir().expect("cwd");
        assert_eq!(
            path_state(&root),
            PathState::NotRegular,
            "a directory is never opened or watched as a document"
        );
        assert_eq!(
            path_state(&root.join("definitely-missing-glisk-watch.html")),
            PathState::Missing
        );
    }
}
