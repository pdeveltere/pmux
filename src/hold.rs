//! The background part of pmux (`pmux hold`): owns every terminal so they keep running when the
//! window closes, crashes or is rebuilt. The window attaches over a Unix socket.
//!
//! One window at a time. On attach the holder sends `Welcome` (terminals + the window's saved
//! layout), then one `Screen` per terminal (its grid as bytes, see `Vt::dump`), then live output.
//!
//! Frame: tag u8, id u32 LE, len u32 LE, payload.
//!
//! Query replies the program expects from "the terminal" (cursor position, device attributes) are
//! answered here, never by the window, so there's exactly one answer.
use crate::pty;
use crate::term::Vt;
use alacritty_terminal::event::{Event as TEvent, WindowSize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bump when the frames or their payloads change: a window only attaches to a holder of its own version.
pub const VERSION: u32 = 2;

/// How often to check which process owns a terminal.
const PROBE_EVERY: Duration = Duration::from_millis(250);
/// An agent with no output for this long counts as idle (same as the window's heuristic).
const WORKING_QUIET: Duration = Duration::from_millis(1500);

// Window → holder.
const SPAWN: u8 = 1;
const INPUT: u8 = 2;
const RESIZE: u8 = 3;
const KILL: u8 = 4;
const LAYOUT: u8 = 5;
/// A terminal's title and explicit state changed (JSON `Meta`).
const META: u8 = 6;
/// The window moved a terminal to another view (JSON `Moved`).
const VIEW: u8 = 7;
// Holder → window.
const WELCOME: u8 = 101;
const SCREEN: u8 = 102;
const OUTPUT: u8 = 103;
const EXIT: u8 = 104;
const AGENT: u8 = 105;
const FAILED: u8 = 106;
/// What the screen says the agent is doing (`detect::Screen`, or empty: nothing known).
const STATE: u8 = 107;
/// Where the terminal is working now (a path): its shell `cd`ed, or its agent's hook said so.
const CWD: u8 = 108;

/// Look at an agent's screen at most this often while it prints.
const DETECT_EVERY: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize, Clone)]
pub struct Spawn {
    pub cwd: PathBuf,
    pub view: PathBuf,
    pub rows: u16,
    pub cols: u16,
    pub env: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TermInfo {
    pub id: u32,
    pub view: PathBuf,
    pub rows: u16,
    pub cols: u16,
    pub agent: Option<String>,
    #[serde(flatten)]
    pub meta: Meta,
    /// The agent produced output while no window was open: it's done, until looked at.
    pub worked: bool,
    /// What the screen says the agent is doing (see `detect`).
    #[serde(default)]
    pub screen: Option<String>,
    /// Where it's working, once that moved after it started (see `CWD`).
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

/// A terminal now belongs to another view, with the names `pmux ls` shows for it.
#[derive(Serialize, Deserialize)]
pub struct Moved {
    pub view: PathBuf,
    pub env: Vec<(String, String)>,
}

/// What `pmux title` and `pmux status` set. The background pmux keeps it, so it's right whether
/// it was set with the window open or closed.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Meta {
    pub title: Option<String>,
    pub state: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Welcome {
    pub version: u32,
    pub layout: String,
    pub terms: Vec<TermInfo>,
    /// Lowest id not yet used (terminals opened with `pmux new` while no window was open).
    pub next_id: u32,
}

pub fn sock_path() -> PathBuf {
    crate::api::sock_dir().join("hold.sock")
}

fn write_frame(w: &mut impl Write, tag: u8, id: u32, payload: &[u8]) -> io::Result<()> {
    let mut b = Vec::with_capacity(9 + payload.len());
    b.push(tag);
    b.extend(id.to_le_bytes());
    b.extend((payload.len() as u32).to_le_bytes());
    b.extend(payload);
    w.write_all(&b)
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, u32, Vec<u8>)> {
    let mut h = [0u8; 9];
    r.read_exact(&mut h)?;
    let id = u32::from_le_bytes(h[1..5].try_into().unwrap());
    let len = u32::from_le_bytes(h[5..9].try_into().unwrap()) as usize;
    let mut p = vec![0u8; len];
    r.read_exact(&mut p)?;
    Ok((h[0], id, p))
}

// ---------------------------------------------------------------- window side

/// Events the holder sends the window, after the welcome.
pub enum Msg {
    Output(u32, Vec<u8>),
    Exit(u32),
    Agent(u32, Option<String>),
    Screen(u32, Option<crate::detect::Screen>),
    Cwd(u32, PathBuf),
    Failed(String),
    Lost,
}

/// The window's connection to the holder. Cheap to clone; writes are whole frames.
#[derive(Clone)]
pub struct Link(Arc<Mutex<UnixStream>>);

impl Link {
    fn send(&self, tag: u8, id: u32, payload: &[u8]) {
        if let Ok(mut s) = self.0.lock() {
            let _ = write_frame(&mut *s, tag, id, payload);
        }
    }
    pub fn spawn(&self, id: u32, s: &Spawn) {
        self.send(SPAWN, id, &serde_json::to_vec(s).unwrap());
    }
    pub fn input(&self, id: u32, bytes: &[u8]) {
        self.send(INPUT, id, bytes);
    }
    pub fn resize(&self, id: u32, rows: u16, cols: u16, xpx: u16, ypx: u16) {
        let p: Vec<u8> = [rows, cols, xpx, ypx].iter().flat_map(|v| v.to_le_bytes()).collect();
        self.send(RESIZE, id, &p);
    }
    pub fn kill(&self, id: u32) {
        self.send(KILL, id, &[]);
    }
    pub fn layout(&self, json: &str) {
        self.send(LAYOUT, 0, json.as_bytes());
    }
    pub fn meta(&self, id: u32, m: &Meta) {
        self.send(META, id, &serde_json::to_vec(m).unwrap());
    }
    pub fn moved(&self, id: u32, m: &Moved) {
        self.send(VIEW, id, &serde_json::to_vec(m).unwrap());
    }

    /// Attach to the holder, starting it if none runs. Returns the link, the welcome, each
    /// terminal's screen, and a function that starts forwarding live events.
    pub fn connect() -> Result<(Link, Welcome, Vec<(u32, Vec<u8>)>, UnixStream), String> {
        let mut s = match UnixStream::connect(sock_path()) {
            Ok(s) => s,
            Err(_) => start()?,
        };
        let (tag, _, p) = read_frame(&mut s).map_err(|e| format!("background pmux: {e}"))?;
        if tag != WELCOME {
            return Err("background pmux: bad welcome".into());
        }
        let w: Welcome = serde_json::from_slice(&p).map_err(|e| format!("background pmux: {e}"))?;
        if w.version != VERSION {
            return Err(format!(
                "background pmux is version {}, this window {VERSION}: close its terminals or `pkill -f 'pmux hold'`",
                w.version
            ));
        }
        let mut screens = Vec::new();
        for _ in 0..w.terms.len() {
            let (tag, id, p) = read_frame(&mut s).map_err(|e| format!("background pmux: {e}"))?;
            if tag == SCREEN {
                screens.push((id, p));
            }
        }
        let r = s.try_clone().map_err(|e| e.to_string())?;
        Ok((Link(Arc::new(Mutex::new(s))), w, screens, r))
    }
}

/// Forward live events from the holder until it goes away.
pub fn pump(mut r: UnixStream, f: impl Fn(Msg) -> bool + Send + 'static) {
    std::thread::spawn(move || {
        while let Ok((tag, id, p)) = read_frame(&mut r) {
            let m = match tag {
                OUTPUT => Msg::Output(id, p),
                EXIT => Msg::Exit(id),
                AGENT => Msg::Agent(id, (!p.is_empty()).then(|| String::from_utf8_lossy(&p).into_owned())),
                STATE => Msg::Screen(id, crate::detect::Screen::parse(&String::from_utf8_lossy(&p))),
                CWD => Msg::Cwd(id, PathBuf::from(String::from_utf8_lossy(&p).into_owned())),
                FAILED => Msg::Failed(String::from_utf8_lossy(&p).into_owned()),
                _ => continue,
            };
            if !f(m) {
                return;
            }
        }
        f(Msg::Lost);
    });
}

/// Start `pmux hold` detached from this process, and connect to it.
fn start() -> Result<UnixStream, String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = crate::api::sock_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let log = File::create(dir.join("hold.log")).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hold").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("start background pmux: {e}"))?;
    std::thread::spawn(move || child.wait());
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(s) = UnixStream::connect(sock_path()) {
            return Ok(s);
        }
        if Instant::now() > until {
            return Err("background pmux didn't start (see /tmp/pmux-$UID/hold.log)".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------- holder side

enum In {
    /// A window connected (generation, stream).
    Attach(u64, UnixStream),
    /// A frame from window `gen`.
    Frame(u64, u8, u32, Vec<u8>),
    /// Window `gen` disconnected.
    Detach(u64),
    Output(u32, Vec<u8>),
    Exit(u32),
    /// A `pmux` command while no window is open.
    Api(crate::api::Request, Sender<crate::api::Reply>),
}

struct Term {
    vt: Vt,
    master: File,
    shell: i32,
    pgid: i32,
    info: TermInfo,
    /// What it was started with (for `pmux new` beside it).
    spawn: Spawn,
    /// Set by the program (OSC 0/2).
    title: Option<String>,
    last_probe: Instant,
    last_output: Instant,
    /// The agent's command line, as it was started (for resuming it).
    argv: Vec<String>,
    /// The agent's session id, reported by its hook (`pmux hook claude`).
    session: Option<String>,
    screen: Option<crate::detect::Screen>,
    last_detect: Instant,
    /// Output arrived since the screen was last looked at.
    detect_due: bool,
    /// The shell's own folder when last probed: a change is a `cd`.
    shell_cwd: Option<PathBuf>,
}

impl Term {
    fn write(&mut self, b: &[u8]) {
        let _ = self.master.write_all(b);
    }
}

struct Holder {
    terms: BTreeMap<u32, Term>,
    win: Option<(u64, UnixStream)>,
    layout: String,
    tx: Sender<In>,
    next_id: u32,
    /// When to look at screens that changed (throttled to `DETECT_EVERY`).
    due: Option<Instant>,
}

/// `pmux hold`: run until the last terminal ends with no window attached.
pub fn run() -> i32 {
    let path = sock_path();
    if UnixStream::connect(&path).is_ok() {
        eprintln!("pmux hold: already running");
        return 1;
    }
    let dir = crate::api::sock_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("pmux hold: {e}");
        return 1;
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_file(&path);
    let l = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pmux hold: {e}");
            return 1;
        }
    };
    let (tx, rx) = channel();
    let atx = tx.clone();
    std::thread::spawn(move || {
        for (gen, s) in (1u64..).zip(l.incoming().flatten()) {
            if atx.send(In::Attach(gen, s)).is_err() {
                return;
            }
        }
    });
    let atx = tx.clone();
    if let Err(e) = crate::api::serve_at(&crate::api::hold_sock(), move |req, rtx| atx.send(In::Api(req, rtx)).is_ok()) {
        eprintln!("pmux hold: {e}");
    }
    let mut h = Holder { terms: BTreeMap::new(), win: None, layout: String::new(), tx, next_id: 1, due: None };
    h.restore();
    loop {
        let ev = match h.due {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(ev) => Some(ev),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            },
            None => match rx.recv() {
                Ok(ev) => Some(ev),
                Err(_) => break,
            },
        };
        match ev {
            Some(ev) => {
                if h.handle(ev) {
                    break;
                }
            }
            None => h.detect_due(),
        }
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(crate::api::hold_sock());
    0
}

impl Holder {
    fn send(&mut self, tag: u8, id: u32, p: &[u8]) {
        if let Some((_, s)) = &mut self.win {
            if write_frame(s, tag, id, p).is_err() {
                self.win = None;
            }
        }
    }

    /// Handle one event; true when it's time to exit.
    fn handle(&mut self, ev: In) -> bool {
        match ev {
            In::Attach(gen, s) => self.attach(gen, s),
            In::Detach(gen) => {
                if self.win.as_ref().is_some_and(|(g, _)| *g == gen) {
                    self.win = None;
                    self.save();
                }
            }
            In::Frame(gen, tag, id, p) => {
                if self.win.as_ref().is_some_and(|(g, _)| *g == gen) {
                    self.frame(tag, id, p);
                }
            }
            In::Output(id, data) => self.output(id, data),
            In::Exit(id) => {
                if self.terms.remove(&id).is_some() {
                    self.send(EXIT, id, &[]);
                    self.save();
                }
            }
            In::Api(req, rtx) => {
                let r = self.api(req);
                let _ = rtx.send(r);
            }
        }
        self.terms.is_empty() && self.win.is_none()
    }

    fn attach(&mut self, gen: u64, s: UnixStream) {
        let Ok(r) = s.try_clone() else { return };
        // A new window replaces the old one.
        if let Some((_, old)) = self.win.take() {
            let _ = old.shutdown(std::net::Shutdown::Both);
        }
        self.win = Some((gen, s));
        let w = Welcome {
            version: VERSION,
            layout: self.layout.clone(),
            terms: self.terms.values().map(|t| t.info.clone()).collect(),
            next_id: self.next_id,
        };
        for t in self.terms.values_mut() {
            t.info.worked = false;
        }
        self.send(WELCOME, 0, &serde_json::to_vec(&w).unwrap());
        let screens: Vec<(u32, Vec<u8>)> = self.terms.iter().map(|(id, t)| (*id, t.vt.dump(t.title.as_deref()))).collect();
        for (id, b) in screens {
            self.send(SCREEN, id, &b);
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut r = r;
            while let Ok((tag, id, p)) = read_frame(&mut r) {
                if tx.send(In::Frame(gen, tag, id, p)).is_err() {
                    return;
                }
            }
            let _ = tx.send(In::Detach(gen));
        });
    }

    fn frame(&mut self, tag: u8, id: u32, p: Vec<u8>) {
        match tag {
            SPAWN => match serde_json::from_slice::<Spawn>(&p).map_err(|e| e.to_string()).and_then(|s| self.spawn(id, s)) {
                Ok(()) => self.save(),
                Err(e) => {
                    self.send(FAILED, id, e.as_bytes());
                    self.send(EXIT, id, &[]);
                }
            },
            INPUT => {
                if let Some(t) = self.terms.get_mut(&id) {
                    t.write(&p);
                }
            }
            RESIZE if p.len() == 8 => {
                let v: Vec<u16> = p.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                if let Some(t) = self.terms.get_mut(&id) {
                    t.info.rows = v[0];
                    t.info.cols = v[1];
                    t.vt.resize(v[1] as usize, v[0] as usize);
                    pty::resize(&t.master, v[0], v[1], v[2], v[3]);
                }
            }
            KILL => {
                if let Some(t) = self.terms.get(&id) {
                    unsafe {
                        libc::kill(-t.pgid, libc::SIGHUP);
                        libc::kill(-t.shell, libc::SIGHUP);
                    }
                }
            }
            LAYOUT => {
                self.layout = String::from_utf8_lossy(&p).into_owned();
                self.save();
            }
            META => {
                if let (Some(t), Ok(m)) = (self.terms.get_mut(&id), serde_json::from_slice(&p)) {
                    t.info.meta = m;
                    self.save();
                }
            }
            VIEW => {
                if let (Some(t), Ok(m)) = (self.terms.get_mut(&id), serde_json::from_slice::<Moved>(&p)) {
                    t.info.view = m.view.clone();
                    t.spawn.view = m.view;
                    for (k, v) in m.env {
                        match t.spawn.env.iter_mut().find(|(n, _)| *n == k) {
                            Some(e) => e.1 = v,
                            None => t.spawn.env.push((k, v)),
                        }
                    }
                    self.save();
                }
            }
            _ => {}
        }
    }

    fn spawn(&mut self, id: u32, s: Spawn) -> Result<(), String> {
        if self.terms.contains_key(&id) {
            return Err(format!("terminal {id} exists"));
        }
        self.next_id = self.next_id.max(id + 1);
        let env: Vec<(&str, String)> = s.env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let p = pty::spawn(&s.cwd, s.rows, s.cols, &env).map_err(|e| e.to_string())?;
        let mut r = p.master.try_clone().map_err(|e| e.to_string())?;
        let tx = self.tx.clone();
        let mut child = p.child;
        let shell = child.id() as i32;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(In::Output(id, buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = child.wait();
            let _ = tx.send(In::Exit(id));
        });
        let info =
            TermInfo { id, view: s.view.clone(), rows: s.rows, cols: s.cols, agent: None, meta: Meta::default(), worked: false, screen: None, cwd: None };
        let long_ago = Instant::now() - Duration::from_secs(60);
        let vt = Vt::new(s.cols as usize, s.rows as usize);
        let t = Term {
            vt,
            master: p.master,
            shell,
            pgid: shell,
            info,
            spawn: s,
            title: None,
            last_probe: long_ago,
            last_output: long_ago,
            argv: vec![],
            session: None,
            screen: None,
            last_detect: long_ago,
            detect_due: false,
            shell_cwd: None,
        };
        self.terms.insert(id, t);
        Ok(())
    }

    fn output(&mut self, id: u32, data: Vec<u8>) {
        let attached = self.win.is_some();
        let Some(t) = self.terms.get_mut(&id) else { return };
        t.last_output = Instant::now();
        if !attached && t.info.agent.is_some() {
            t.info.worked = true;
        }
        for e in t.vt.feed(&data) {
            match e {
                TEvent::PtyWrite(s) => t.write(s.as_bytes()),
                TEvent::Title(s) => t.title = Some(s),
                TEvent::ResetTitle => t.title = None,
                // With a window attached it answers these (it knows colours and cell size).
                TEvent::ColorRequest(i, f) if !attached => {
                    let rgb = t.vt.term.colors()[i].unwrap_or_else(|| crate::ui::default_color(i));
                    t.write(f(rgb).as_bytes());
                }
                TEvent::TextAreaSizeRequest(f) if !attached => {
                    let ws = WindowSize { num_lines: t.info.rows, num_cols: t.info.cols, cell_width: 0, cell_height: 0 };
                    t.write(f(ws).as_bytes());
                }
                _ => {}
            }
        }
        t.detect_due = true;
        let now = Instant::now();
        let next = t.last_detect + DETECT_EVERY;
        self.send(OUTPUT, id, &data);
        if now - self.terms[&id].last_probe > PROBE_EVERY {
            self.probe(id);
        }
        if now >= next {
            self.detect(id);
        } else {
            self.due = Some(self.due.map_or(next, |d| d.min(next)));
        }
    }

    /// Which process owns the terminal: detects agents starting and exiting.
    fn probe(&mut self, id: u32) {
        let Some(t) = self.terms.get_mut(&id) else { return };
        let mut changed = None;
        t.last_probe = Instant::now();
        {
            let pg = pty::fg_pgrp(&t.master);
            if pg != t.pgid {
                t.pgid = pg;
                let a = pty::proc_path(pg).as_deref().and_then(pty::agent_of).map(String::from);
                if a != t.info.agent {
                    t.argv = if a.is_some() { pty::proc_args(pg) } else { vec![] };
                    t.info.agent = a.clone();
                    t.info.meta.state = None;
                    t.info.worked = false;
                    // The hook may report the session before the agent is seen here: keep it
                    // until the agent exits.
                    if a.is_none() {
                        t.session = None;
                    }
                    t.screen = None;
                    changed = Some(a);
                }
            }
        }
        if let Some(a) = changed {
            self.send(AGENT, id, a.as_deref().unwrap_or("").as_bytes());
            self.send(STATE, id, &[]);
            self.save();
        }
        // At the shell's prompt: a `cd` moves where the terminal works. Only a change counts, so
        // an agent exiting (its shell still where it started) doesn't move anything.
        let Some(t) = self.terms.get_mut(&id) else { return };
        if t.info.agent.is_none() && t.pgid == t.shell {
            if let Some(cwd) = pty::proc_cwd(t.shell) {
                if t.shell_cwd.replace(cwd.clone()).is_some_and(|old| old != cwd) {
                    self.set_cwd(id, cwd);
                }
            }
        }
    }

    /// The terminal works in `dir` now: tell the window, which moves its tab to that worktree.
    fn set_cwd(&mut self, id: u32, dir: PathBuf) {
        let Some(t) = self.terms.get_mut(&id) else { return };
        if t.info.cwd.as_ref() == Some(&dir) {
            return;
        }
        t.info.cwd = Some(dir.clone());
        self.send(CWD, id, dir.as_os_str().as_encoded_bytes());
    }

    /// Look at the screens that changed since they were last looked at.
    fn detect_due(&mut self) {
        self.due = None;
        let ids: Vec<u32> = self.terms.iter().filter(|(_, t)| t.detect_due).map(|(id, _)| *id).collect();
        for id in ids {
            self.detect(id);
        }
    }

    /// What the agent's screen says it's doing; tell the window when that changes.
    fn detect(&mut self, id: u32) {
        // An agent that just started may have printed only once: check it's still the same program.
        if self.terms.get(&id).is_some_and(|t| t.last_probe.elapsed() > DETECT_EVERY) {
            self.probe(id);
        }
        let Some(t) = self.terms.get_mut(&id) else { return };
        t.detect_due = false;
        t.last_detect = Instant::now();
        let Some(m) = t.info.agent.as_deref().and_then(crate::detect::manifest) else { return };
        // The visible screen, not the scrollback the user may have scrolled to.
        let lines: Vec<String> = t.vt.text().lines().map(String::from).collect();
        let Some((s, _)) = m.detect(&lines, t.title.as_deref().unwrap_or("")) else { return };
        if t.screen != Some(s) {
            t.screen = Some(s);
            t.info.screen = Some(s.as_str().to_string());
            self.send(STATE, id, s.as_str().as_bytes());
        }
    }
}

// ---------------------------------------------------------------- commands with no window

const STATUS_ARGS: &str = "working|waiting|idle|done|error|clear";

impl Term {
    fn kind(&self) -> &str {
        self.info.agent.as_deref().unwrap_or("shell")
    }
    /// Same rules as the window: explicit state, else the screen; output only counts until the
    /// screen has given a verdict.
    fn state(&self) -> &str {
        use crate::detect::Screen;
        if let Some(s) = &self.info.meta.state {
            return s;
        }
        match self.screen {
            Some(Screen::Blocked) => return "waiting",
            Some(Screen::Working) => return "working",
            _ => {}
        }
        if self.info.agent.is_none() {
            "-"
        } else if self.screen.is_none() && self.last_output.elapsed() < WORKING_QUIET {
            "working"
        } else if self.info.worked {
            "done"
        } else {
            "idle"
        }
    }
    fn label(&self) -> String {
        self.info.meta.title.clone().or_else(|| self.title.clone()).unwrap_or_else(|| self.kind().to_string())
    }
    fn place(&self) -> String {
        let env = |k: &str| self.spawn.env.iter().find(|(n, _)| n == k).map_or("", |(_, v)| v.as_str()).to_string();
        format!("{}/{}/{}", env("PMUX_WORKSPACE"), env("PMUX_PROJECT"), env("PMUX_WORKTREE"))
    }
    fn line(&self, me: Option<u32>) -> String {
        let here = if Some(self.info.id) == me { "  ◀ here" } else { "" };
        format!("%{:<3} {:<8} {:<8} {:<36} {}{}", self.info.id, self.kind(), self.state(), self.place(), self.label(), here)
    }
}

impl Holder {
    /// A terminal by `%N`, `N`, `self` (the caller) or title.
    fn resolve(&self, r: &str, caller: Option<&str>) -> Option<u32> {
        let r = if r == "self" { caller? } else { r };
        if let Ok(n) = r.trim_start_matches('%').parse::<u32>() {
            return self.terms.contains_key(&n).then_some(n);
        }
        self.terms.values().find(|t| t.label() == r).map(|t| t.info.id)
    }

    /// `pmux <command>` while no window is open: everything about terminals; nothing about the
    /// sidebar, editors or the screen.
    fn api(&mut self, req: crate::api::Request) -> crate::api::Reply {
        use crate::api::Reply;
        let caller = req.caller.as_deref();
        let me = caller.and_then(|c| self.resolve(c, None));
        let a: Vec<&str> = req.args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["ls", ..] | ["tree", ..] => Reply::ok(self.terms.values().map(|t| t.line(me) + "\n").collect::<String>()),
            ["whoami", ..] => match me.and_then(|id| self.terms.get(&id)) {
                Some(t) => Reply::ok(t.line(None)),
                None => Reply::err("not inside a pmux pane"),
            },
            ["read", r, rest @ ..] => {
                let Some(id) = self.resolve(r, caller) else { return Reply::err(format!("no pane {r}")) };
                let n: usize = rest.first().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
                let text = self.terms[&id].vt.text();
                let lines: Vec<&str> = text.trim_end().lines().collect();
                Reply::ok(lines[lines.len().saturating_sub(n)..].join("\n"))
            }
            ["send", r, text @ ..] if !text.is_empty() => {
                let Some(id) = self.resolve(r, caller) else { return Reply::err(format!("no pane {r}")) };
                self.send_text(id, &text.join(" "));
                Reply::ok("")
            }
            ["status", st, rest @ ..] => {
                let Some(id) = rest.first().and_then(|r| self.resolve(r, caller)).or(me) else { return Reply::err("no target pane") };
                let state = match *st {
                    "clear" => None,
                    "working" | "waiting" | "idle" | "done" | "error" => Some(st.to_string()),
                    _ => return Reply::err(format!("state must be one of {STATUS_ARGS}")),
                };
                if let Some(t) = self.terms.get_mut(&id) {
                    t.info.meta.state = state;
                }
                Reply::ok("")
            }
            ["title", text @ ..] if !text.is_empty() => match me.and_then(|id| self.terms.get_mut(&id)) {
                Some(t) => {
                    t.info.meta.title = Some(text.join(" "));
                    Reply::ok("")
                }
                None => Reply::err("not inside a pmux pane"),
            },
            // From the agent's own hook: which session runs in this terminal (for resuming it).
            ["agent-session", _agent, sid] => match me.and_then(|id| self.terms.get_mut(&id)) {
                Some(t) => {
                    t.session = Some(sid.to_string());
                    self.save();
                    Reply::ok("")
                }
                None => Reply::err("not inside a pmux pane"),
            },
            // From the agent's hook: it works in another folder now (`cd`, `EnterWorktree`).
            ["agent-cwd", _agent, dir] => match me {
                Some(id) => {
                    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| PathBuf::from(dir));
                    self.set_cwd(id, dir);
                    Reply::ok("")
                }
                None => Reply::err("not inside a pmux pane"),
            },
            ["new", rest @ ..] => {
                let cmd = match rest {
                    ["--tab", cmd @ ..] => cmd,
                    cmd => cmd,
                };
                self.api_new(me, cmd)
            }
            [c, ..] => Reply::err(format!("`pmux {c}` needs the pmux window open (terminal commands work without it)")),
            [] => Reply::err("no command"),
        }
    }

    /// Type text, then Enter a moment later so agents don't take it as part of a paste.
    fn send_text(&mut self, id: u32, text: &str) {
        let Some(t) = self.terms.get_mut(&id) else { return };
        t.write(text.as_bytes());
        if let Ok(mut f) = t.master.try_clone() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                let _ = f.write_all(b"\r");
            });
        }
    }

    /// `pmux new`: a terminal beside the caller (same folder and size); the next window shows it
    /// in a new tab.
    fn api_new(&mut self, me: Option<u32>, cmd: &[&str]) -> crate::api::Reply {
        use crate::api::Reply;
        let Some(t) = me.and_then(|id| self.terms.get(&id)) else { return Reply::err("`pmux new` from outside a pane needs the pmux window open") };
        let id = self.next_id;
        let mut s = t.spawn.clone();
        s.rows = t.info.rows;
        s.cols = t.info.cols;
        for (k, v) in &mut s.env {
            if k == "PMUX_PANE" {
                *v = format!("%{id}");
            }
        }
        if let Err(e) = self.spawn(id, s) {
            return Reply::err(e);
        }
        if !cmd.is_empty() {
            if let Some(t) = self.terms.get_mut(&id) {
                t.write(format!("{}\r", cmd.join(" ")).as_bytes());
            }
        }
        Reply::ok(format!("%{id}"))
    }
}

// ---------------------------------------------------------------- saved on disk

/// Where the background pmux keeps its terminals, so a new one (after a crash, an upgrade or a
/// reboot) can open them again and resume their agents.
fn session_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("pmux/session.json")
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    next_id: u32,
    layout: String,
    terms: Vec<SavedTerm>,
}

#[derive(Serialize, Deserialize)]
struct SavedTerm {
    id: u32,
    /// With `cwd` where the shell was last.
    spawn: Spawn,
    meta: Meta,
    agent: Option<String>,
    argv: Vec<String>,
    session: Option<String>,
}

/// The command that brings an agent back in a new shell, if we know how.
fn resume_cmd(agent: &str, argv: &[String], session: Option<&str>) -> Option<String> {
    match agent {
        "claude" => {
            let argv = if argv.is_empty() { vec!["claude".to_string()] } else { argv.to_vec() };
            Some(crate::import::claude_cmd(&argv, session.map(String::from)))
        }
        _ => None,
    }
}

impl Holder {
    /// Write the terminals to disk (on every change that matters to a restore).
    fn save(&mut self) {
        // No hook installed: Claude also records its session id under its pid.
        for t in self.terms.values_mut() {
            if t.session.is_none() && t.info.agent.as_deref() == Some("claude") {
                t.session = crate::import::session_id(t.pgid as i64);
            }
        }
        let terms = self
            .terms
            .values()
            .map(|t| {
                let mut spawn = t.spawn.clone();
                if let Some(cwd) = pty::proc_cwd(t.shell) {
                    spawn.cwd = cwd;
                }
                spawn.rows = t.info.rows;
                spawn.cols = t.info.cols;
                SavedTerm { id: t.info.id, spawn, meta: t.info.meta.clone(), agent: t.info.agent.clone(), argv: t.argv.clone(), session: t.session.clone() }
            })
            .collect();
        let s = Saved { next_id: self.next_id, layout: self.layout.clone(), terms };
        let path = session_file();
        let tmp = path.with_extension("tmp");
        let ok = path.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && std::fs::write(&tmp, serde_json::to_vec(&s).unwrap()).is_ok()
            && std::fs::rename(&tmp, &path).is_ok();
        if !ok {
            eprintln!("pmux hold: can't save {}", path.display());
        }
    }

    /// Starting up: open the terminals the last background pmux had, and resume their agents.
    fn restore(&mut self) {
        let Ok(b) = std::fs::read(session_file()) else { return };
        let Ok(saved) = serde_json::from_slice::<Saved>(&b) else { return };
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        for st in saved.terms {
            let mut spawn = st.spawn;
            if !spawn.cwd.is_dir() {
                spawn.cwd = if spawn.view.is_dir() { spawn.view.clone() } else { home.clone() };
            }
            if let Err(e) = self.spawn(st.id, spawn) {
                eprintln!("pmux hold: restore %{}: {e}", st.id);
                continue;
            }
            let Some(t) = self.terms.get_mut(&st.id) else { continue };
            t.info.meta = st.meta;
            t.session = st.session.clone();
            if let Some(cmd) = st.agent.as_deref().and_then(|a| resume_cmd(a, &st.argv, st.session.as_deref())) {
                t.write(format!("{cmd}\r").as_bytes());
            }
        }
        self.layout = saved.layout;
        self.next_id = self.next_id.max(saved.next_id);
        eprintln!("pmux hold: restored {} terminals", self.terms.len());
    }
}
