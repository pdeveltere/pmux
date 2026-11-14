use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

pub struct Pty {
    pub master: File,
    pub child: Child,
}

fn ws(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

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

pub fn resize(f: &File, rows: u16, cols: u16, xpx: u16, ypx: u16) {
    let w = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: xpx, ws_ypixel: ypx };
    unsafe { libc::ioctl(f.as_raw_fd(), libc::TIOCSWINSZ, &w) };
}

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

pub fn agent_of(path: &str) -> Option<&'static str> {
    let p = path.to_ascii_lowercase();
    ["claude", "codex", "gemini", "opencode", "aider", "amp", "cursor-agent"]
        .into_iter()
        .find(|a| p.contains(a))
}
