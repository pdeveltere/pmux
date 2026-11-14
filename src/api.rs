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

pub fn sock_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/pmux-{}", unsafe { libc::getuid() }))
}

pub fn listen(tx: Tx) -> std::io::Result<PathBuf> {
    let dir = sock_dir();
    std::fs::create_dir_all(&dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path)?;
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || serve(s, tx));
        }
    });
    Ok(path)
}

fn serve(s: UnixStream, tx: Tx) {
    let mut line = String::new();
    let mut r = BufReader::new(&s);
    if r.read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(req) => {
            let (rtx, rrx) = channel();
            if tx.send_event(Ev::Api(req, rtx)).is_err() {
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
    if let Ok(p) = std::env::var("PMUX_SOCKET") {
        return UnixStream::connect(p).ok();
    }
    // Outside pmux: use the most recent live instance.
    let mut socks: Vec<_> = std::fs::read_dir(sock_dir())
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    socks.sort();
    socks.into_iter().rev().find_map(|(_, p)| UnixStream::connect(p).ok())
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
        eprintln!("pmux: no running pmux found (open one with `pmux`)");
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
