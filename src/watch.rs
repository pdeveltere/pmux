//! File watching for git metadata. macOS: kqueue, fully event-driven (no polling).
use crate::app::{Ev, Tx};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct Watcher {
    #[cfg(target_os = "macos")]
    kq: i32,
    paths: Arc<Mutex<Vec<PathBuf>>>,
}

#[cfg(target_os = "macos")]
fn kev(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent { ident, filter, flags, fflags, data: 0, udata: std::ptr::null_mut() }
}

impl Watcher {
    #[cfg(target_os = "macos")]
    pub fn new(tx: Tx) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let paths: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let kq = unsafe { libc::kqueue() };
        let user = kev(1, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0);
        unsafe { libc::kevent(kq, &user, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        let shared = paths.clone();
        std::thread::spawn(move || {
            let mut fds: Vec<i32> = Vec::new();
            let mut out = [kev(0, 0, 0, 0); 32];
            loop {
                let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), 32, std::ptr::null()) };
                if n < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                let changed = out[..n as usize].iter().any(|e| e.filter != libc::EVFILT_USER);
                // Re-arm on every wake: new worktree dirs may have appeared.
                for fd in fds.drain(..) {
                    unsafe { libc::close(fd) };
                }
                for p in shared.lock().unwrap().iter() {
                    let mut c = p.as_os_str().as_bytes().to_vec();
                    c.push(0);
                    let fd = unsafe { libc::open(c.as_ptr() as *const _, libc::O_EVTONLY) };
                    if fd < 0 {
                        continue;
                    }
                    let flags = libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_EXTEND;
                    let e = kev(fd as usize, libc::EVFILT_VNODE, libc::EV_ADD | libc::EV_CLEAR, flags);
                    unsafe { libc::kevent(kq, &e, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
                    fds.push(fd);
                }
                if changed && tx.send_event(Ev::Git).is_err() {
                    return;
                }
            }
        });
        Watcher { kq, paths }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn new(tx: Tx) -> Self {
        // Prototype fallback: coarse poll. Linux will use inotify.
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            if tx.send_event(Ev::Git).is_err() {
                return;
            }
        });
        Watcher { paths: Arc::default() }
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
