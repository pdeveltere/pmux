//! PTYs and processes: spawn a login shell on a new PTY, resize it, and see which program
//! (agent) runs in its foreground. Unix only (openpty, process groups).
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

/// A spawned shell: the PTY master (read output / write input) and the child process.
pub struct Pty {
    pub master: File,
    pub child: Child,
}

fn ws(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

/// Open a PTY of `rows`×`cols` and start `$SHELL -l` in `cwd` on it, as the session leader with
/// the PTY as its controlling terminal. `env` is added to the inherited environment.
pub fn spawn(cwd: &Path, rows: u16, cols: u16, env: &[(&str, String)]) -> io::Result<Pty> {
    let (mut m, mut s) = (0, 0);
    let w = ws(rows, cols);
    if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &w as *const _ as *mut _) } != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC) };
    let master = unsafe { File::from_raw_fd(m) };
    let slave = unsafe { File::from_raw_fd(s) };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut cmd = Command::new(&shell);
    cmd.arg("-l")
        .current_dir(cwd)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave);
    // Don't leak a parent Claude Code session's env (pmux is often launched from one):
    // a `claude` started in a pane would think it's a child session and disable transcripts.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("CLAUDE") || k == "AI_AGENT" {
            cmd.env_remove(&*k);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(Pty { master, child })
}

/// Tell the PTY its new size (the kernel sends SIGWINCH to the foreground job).
pub fn resize(f: &File, rows: u16, cols: u16, xpx: u16, ypx: u16) {
    let w = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: xpx, ws_ypixel: ypx };
    unsafe { libc::ioctl(f.as_raw_fd(), libc::TIOCSWINSZ, &w) };
}

/// The terminal's foreground process group (the job that currently owns it).
pub fn fg_pgrp(f: &File) -> i32 {
    unsafe { libc::tcgetpgrp(f.as_raw_fd()) }
}

#[cfg(target_os = "macos")]
pub fn proc_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn proc_path(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Which known coding agent an executable path belongs to, by name.
pub fn agent_of(path: &str) -> Option<&'static str> {
    let p = path.to_ascii_lowercase();
    ["claude", "codex", "gemini", "opencode", "aider", "amp", "cursor-agent"]
        .into_iter()
        .find(|a| p.contains(a))
}

/// A process's command line.
#[cfg(target_os = "macos")]
pub fn proc_args(pid: i32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = buf.len();
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut _, &mut len, std::ptr::null_mut(), 0) } != 0 || len < 4 {
        return vec![];
    }
    // argc, the executable path, padding NULs, then argv.
    let argc = i32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
    let rest = &buf[4..len];
    let start = rest.iter().position(|b| *b == 0).map_or(rest.len(), |i| i);
    let rest = &rest[start..];
    let first = rest.iter().position(|b| *b != 0).unwrap_or(rest.len());
    rest[first..].split(|b| *b == 0).take(argc).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

#[cfg(not(target_os = "macos"))]
pub fn proc_args(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
        .unwrap_or_default()
}

/// A process's current folder.
#[cfg(target_os = "macos")]
pub fn proc_cwd(pid: i32) -> Option<std::path::PathBuf> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut _, size) };
    if n != size {
        return None;
    }
    let p = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const _) };
    Some(std::path::PathBuf::from(p.to_string_lossy().into_owned()))
}

#[cfg(not(target_os = "macos"))]
pub fn proc_cwd(pid: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_args_and_cwd() {
        let me = std::process::id() as i32;
        assert!(!super::proc_args(me).is_empty());
        assert_eq!(super::proc_cwd(me), std::env::current_dir().ok());
    }
}
