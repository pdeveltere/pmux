//! Unix-socket API: one JSON request per connection, one JSON reply.
use crate::app::{Ev, Tx};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::channel;

pub const GUIDE: &str = "pmux guide (for agents)

You are running inside pmux if $PMUX=1. Your pane is $PMUX_PANE.
Your project is $PMUX_PROJECT, worktree $PMUX_WORKTREE, workspace $PMUX_WORKSPACE.

Orient (only when the task involves other panes):
  pmux tree                 see all workspaces, worktrees and panes; yours is marked ◀ here
  pmux ls                   one line per pane: id, agent, state, location, title

Identity:
  pmux title \"Review auth\"  2-4 words, role first, distinct from siblings

Coordinate:
  pmux read %2 40           last 40 lines of pane %2
  pmux send %2 \"text\"       type text + Enter into pane %2 (a poke, not durable)
  pmux new claude           open a pane next to you and run a command in it
  pmux open src/x.rs        show a file to the operator in an editor tab (no focus change)

State (the sidebar shows it to the operator):
  pmux status working|waiting|idle|done|error|clear
";

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub caller: Option<String>,
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    pub out: String,
}

impl Reply {
    pub fn ok(out: impl Into<String>) -> Self {
        Reply { ok: true, out: out.into() }
    }
    pub fn err(out: impl Into<String>) -> Self {
        Reply { ok: false, out: out.into() }
    }
}

/// Sockets live here. `PMUX_RUNTIME_DIR` moves them (a separate pmux, e.g. for testing).
pub fn sock_dir() -> PathBuf {
    std::env::var_os("PMUX_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("/tmp/pmux-{}", unsafe { libc::getuid() })))
}

pub fn listen(tx: Tx) -> std::io::Result<PathBuf> {
    let dir = sock_dir();
    std::fs::create_dir_all(&dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    // One window, at a fixed path: terminals outlive the window, so their $PMUX_SOCKET must
    // still reach the next one.
    let path = dir.join("pmux.sock");
    if UnixStream::connect(&path).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "pmux is already open"));
    }
    serve_at(&path, move |req, rtx| tx.send_event(Ev::Api(req, rtx)).is_ok())?;
    Ok(path)
}

/// Where the background pmux answers commands while no window is open.
pub fn hold_sock() -> PathBuf {
    sock_dir().join("hold-api.sock")
}

/// Answer requests on `path`: `on` gets each one with a channel for its reply (false: shutting down).
pub fn serve_at(path: &std::path::Path, on: impl Fn(Request, std::sync::mpsc::Sender<Reply>) -> bool + Send + Clone + 'static) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let on = on.clone();
            std::thread::spawn(move || serve(s, on));
        }
    });
    Ok(())
}

fn serve(s: UnixStream, on: impl Fn(Request, std::sync::mpsc::Sender<Reply>) -> bool) {
    let mut line = String::new();
    let mut r = BufReader::new(&s);
    if r.read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(req) => {
            let (rtx, rrx) = channel();
            if !on(req, rtx) {
                return;
            }
            rrx.recv().unwrap_or_else(|_| Reply::err("pmux closed"))
        }
        Err(e) => Reply::err(format!("bad request: {e}")),
    };
    let mut w = &s;
    let _ = w.write_all(serde_json::to_string(&reply).unwrap().as_bytes());
}

fn connect() -> Option<UnixStream> {
    let p = std::env::var("PMUX_SOCKET").map(PathBuf::from).unwrap_or_else(|_| sock_dir().join("pmux.sock"));
    // No window open: the background pmux answers what it can.
    UnixStream::connect(p).or_else(|_| UnixStream::connect(hold_sock())).ok()
}

pub fn client(mut args: Vec<String>) -> i32 {
    // `open` takes a path relative to the caller's cwd; the app has its own cwd.
    if args.first().map(String::as_str) == Some("open") {
        if let Some(p) = args.get_mut(1) {
            if let Ok(abs) = std::fs::canonicalize(&*p) {
                *p = abs.display().to_string();
            }
        }
    }
    let Some(mut s) = connect() else {
        eprintln!("pmux: no pmux window open (open one with `pmux`)");
        return 1;
    };
    let req = Request { caller: std::env::var("PMUX_PANE").ok(), args };
    let mut msg = serde_json::to_string(&req).unwrap();
    msg.push('\n');
    if s.write_all(msg.as_bytes()).is_err() {
        return 1;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    match serde_json::from_str::<Reply>(&buf) {
        Ok(r) if r.ok => {
            print!("{}", r.out);
            if !r.out.is_empty() && !r.out.ends_with('\n') {
                println!();
            }
            0
        }
        Ok(r) => {
            eprintln!("pmux: {}", r.out);
            1
        }
        Err(_) => {
            eprintln!("pmux: bad reply");
            1
        }
    }
}
