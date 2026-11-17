//! File watching (git metadata, explorer folders). macOS: kqueue, fully event-driven (no polling).
use crate::app::{Ev, Tx};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// After a change, wait until the paths have been quiet this long before reporting: one git
/// command touches several of them in a burst.
#[cfg(target_os = "macos")]
const SETTLE: std::time::Duration = std::time::Duration::from_millis(100);
/// …but report a continuous stream of changes at least this often.
#[cfg(target_os = "macos")]
const SETTLE_MAX: std::time::Duration = std::time::Duration::from_millis(500);

/// Watches a set of paths on a background thread and sends `ev(changed paths)` when any change.
/// On macOS each path is a kqueue vnode watch; replacing the set wakes the thread to re-sync.
pub struct Watcher {
    #[cfg(target_os = "macos")]
    kq: i32,
    paths: Arc<Mutex<Vec<PathBuf>>>,
}

#[cfg(target_os = "macos")]
fn kev(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent { ident, filter, flags, fflags, data: 0, udata: std::ptr::null_mut() }
}

/// kqueue state on the watcher thread: an open fd per watched path that exists.
#[cfg(target_os = "macos")]
struct Armed {
    kq: i32,
    fds: std::collections::HashMap<PathBuf, i32>,
    paths: std::collections::HashMap<i32, PathBuf>,
}

#[cfg(target_os = "macos")]
impl Armed {
    fn arm(&mut self, p: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        let mut c = p.as_os_str().as_bytes().to_vec();
        c.push(0);
        let fd = unsafe { libc::open(c.as_ptr() as *const _, libc::O_EVTONLY) };
        if fd < 0 {
            return; // doesn't exist (yet): retried on the next wake
        }
        let flags = libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_EXTEND;
        let e = kev(fd as usize, libc::EVFILT_VNODE, libc::EV_ADD | libc::EV_CLEAR, flags);
        unsafe { libc::kevent(self.kq, &e, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        self.fds.insert(p.to_path_buf(), fd);
        self.paths.insert(fd, p.to_path_buf());
    }

    fn disarm(&mut self, p: &PathBuf) {
        if let Some(fd) = self.fds.remove(p) {
            self.paths.remove(&fd);
            unsafe { libc::close(fd) }; // closing drops its kevent
        }
    }

    /// Match the wanted set: drop paths no longer wanted, arm new or reappeared ones.
    fn sync(&mut self, want: &[PathBuf]) {
        let stale: Vec<PathBuf> = self.fds.keys().filter(|p| !want.contains(p)).cloned().collect();
        for p in &stale {
            self.disarm(p);
        }
        for p in want {
            if !self.fds.contains_key(p) {
                self.arm(p);
            }
        }
    }
}

impl Watcher {
    #[cfg(target_os = "macos")]
    /// Watch paths set with `set`; each burst of changes sends `ev(changed)`.
    pub fn new(tx: Tx, ev: fn(Vec<PathBuf>) -> Ev) -> Self {
        let paths: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let kq = unsafe { libc::kqueue() };
        let user = kev(1, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0);
        unsafe { libc::kevent(kq, &user, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        let shared = paths.clone();
        std::thread::spawn(move || {
            let mut armed = Armed { kq, fds: Default::default(), paths: Default::default() };
            let mut out = [kev(0, 0, 0, 0); 32];
            let settle = libc::timespec { tv_sec: 0, tv_nsec: SETTLE.as_nanos() as _ };
            loop {
                let mut changed: Vec<PathBuf> = Vec::new();
                let mut gone: Vec<PathBuf> = Vec::new();
                let mut first: Option<std::time::Instant> = None;
                // Block for the first event, then gather the rest of the burst.
                loop {
                    let timeout = if first.is_some() { &settle as *const _ } else { std::ptr::null() };
                    let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 32, timeout) };
                    if n < 0 {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        return;
                    }
                    if n == 0 {
                        break; // quiet: the burst is over
                    }
                    first.get_or_insert_with(std::time::Instant::now);
                    for e in &out[..n as usize] {
                        if e.filter == libc::EVFILT_USER {
                            continue;
                        }
                        if let Some(p) = armed.paths.get(&(e.ident as i32)) {
                            if !changed.contains(p) {
                                changed.push(p.clone());
                            }
                            // Replaced (git writes via lock file + rename) or deleted: the fd
                            // watches a dead inode now, so re-open the path.
                            if e.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0 {
                                gone.push(p.clone());
                            }
                        }
                    }
                    if first.is_some_and(|t| t.elapsed() >= SETTLE_MAX) {
                        break;
                    }
                }
                for p in &gone {
                    armed.disarm(p);
                }
                armed.sync(&shared.lock().unwrap());
                if !changed.is_empty() && tx.send_event(ev(changed)).is_err() {
                    return;
                }
            }
        });
        Watcher { kq, paths }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn new(tx: Tx, ev: fn(Vec<PathBuf>) -> Ev) -> Self {
        // Prototype fallback: coarse poll, reporting every path. Linux will use inotify.
        let paths: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let shared = paths.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let all = shared.lock().unwrap().clone();
            if tx.send_event(ev(all)).is_err() {
                return;
            }
        });
        Watcher { paths }
    }

    pub fn set(&self, paths: Vec<PathBuf>) {
        *self.paths.lock().unwrap() = paths;
        #[cfg(target_os = "macos")]
        {
            let e = kev(1, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
            unsafe { libc::kevent(self.kq, &e, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        }
    }
}
