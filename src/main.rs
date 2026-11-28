mod api;
mod app;
mod config;
mod detect;
mod editor;
mod files;
mod finder;
mod git;
mod gpu;
mod hold;
mod hook;
mod icons;
mod import;
mod input;
#[cfg(target_os = "macos")]
mod macmenu;
#[cfg(target_os = "macos")]
mod notify;
mod platform;
mod pty;
mod search;
mod settings;
mod syntax;
mod term;
mod text;
mod ui;
mod util;
mod watch;

use app::{App, Ev};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const USAGE: &str = "pmux: agent-aware terminal multiplexer (prototype)

usage:
  pmux                      open the pmux window
  pmux ls                   list panes
  pmux tree                 workspaces / projects / worktrees / panes
  pmux whoami               this pane
  pmux read <pane> [n]      last n lines of a pane's screen
  pmux send <pane> <text>   type text + Enter into a pane
  pmux new [cmd...]         open a pane next to this one (optionally run cmd)
  pmux new --tab [cmd...]   open a pane in a new tab of this worktree
  pmux open <file>          open a file in an editor tab of this worktree
  pmux status <state>       set my state: working|waiting|idle|done|error|clear
  pmux title <text>         set my title
  pmux ws new <dir>                    add a workspace (a folder)
  pmux project add <dir> [-w <ws>]     add a project (default: my workspace)
  pmux ws rm <name> [--force]          close a workspace (sidebar only)
  pmux project rm <dir|name> [--force] close a project (sidebar only; files untouched)
  pmux worktree rm <dir> [--force]     delete a linked worktree (git worktree remove)
  pmux guide                how agents should use pmux
  pmux import herdr [--run] open Herdr's live terminals here (claude sessions resumed)
  pmux integration install claude   let Claude report its session (for resume after restarts)

Terminals keep running when the window closes. With no window open, the terminal
commands (ls, read, send, status, title, new) still work.

panes: %N, N, self, or a title

keys:
  ⌘D split   ⌘W close   ⌘[ ⌘] focus   ⌘K clear   ⌘V paste   ⌘E sidebar focus   ⌘B hide sidebar
  ⌘↑ ⌘↓ prev/next worktree   ⌘N next pane needing you   ⌘= ⌘- ⌘0 font size   ⌘Q quit
  C-a prefix (tmux style): | split  h/l focus  x close  c clear  j/k worktree  n attention  Tab sidebar  q quit
sidebar:
  j/k move  Enter open  h/l fold  a add project  w new worktree  W new workspace (header +: one at ~)
  D remove worktree  x close (or hover ×)  Esc back
";

struct Runner {
    proxy: app::Tx,
    socket: std::path::PathBuf,
    app: Option<App>,
    window: Option<Arc<Window>>,
    gpu: Option<gpu::Gpu>,
    fonts: Option<text::Fonts>,
    /// Reused between frames.
    bufs: ui::FrameBufs,
    /// The window can't be seen (minimized, covered, on another Space): no frames are drawn
    /// until it can be, which redraws whatever changed meanwhile.
    occluded: bool,
}

impl Runner {
    fn redraw(&mut self) {
        let (Some(app), Some(gpu), Some(fonts)) = (self.app.as_mut(), self.gpu.as_mut(), self.fonts.as_mut()) else {
            return;
        };
        if let Some(z) = app.zoom.take() {
            fonts.mono_pt = if z == 0 { text::MONO_PT } else { (fonts.mono_pt + z as f32).clamp(8.0, 32.0) };
            fonts.reset();
        }
        app.cell_w = fonts.cell_w;
        app.cell_h = fonts.cell_h;
        app.apply_layout();
        ui::frame(app, fonts, &mut self.bufs);
        if fonts.take_overflow() {
            ui::frame(app, fonts, &mut self.bufs);
        }
        let insts = &self.bufs.insts;
        gpu.set_atlas_size(fonts.atlas);
        for u in fonts.uploads.drain(..) {
            gpu.upload(u.x, u.y, u.w, u.h, &u.rgba);
        }
        let bg = ui::BG;
        if !gpu.render(insts, [bg[0], bg[1], bg[2]]) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if let Some((path, rtx)) = app.snapshot.take() {
            let (w, h, px, bgra) = gpu.snapshot(insts, [bg[0], bg[1], bg[2]]);
            let r = match gpu::write_bmp(&path, w, h, &px, bgra) {
                Ok(()) => api::Reply::ok(format!("{path} ({w}x{h})")),
                Err(e) => api::Reply::err(e.to_string()),
            };
            let _ = rtx.send(r);
        }
    }

    fn shutdown(&mut self, el: &ActiveEventLoop) {
        // Terminals keep running in the background pmux; the next window picks them up.
        if let Some(app) = self.app.as_mut() {
            app.sync_layout();
        }
        let _ = std::fs::remove_file(&self.socket);
        el.exit();
    }
}

impl ApplicationHandler<Ev> for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("pmux").with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = Arc::new(el.create_window(attrs).expect("window"));
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let fonts = text::Fonts::new(scale);
        let mut app = App::new(self.proxy.clone(), self.socket.clone());
        app.scale = scale;
        app.w = size.width as f32;
        app.h = size.height as f32;
        app.cell_w = fonts.cell_w;
        app.cell_h = fonts.cell_h;
        match app.attach() {
            Ok(Some(view)) => app.open_view(view),
            Ok(None) => app.open_cwd(),
            Err(e) => {
                app.say(e);
                app.open_cwd();
            }
        }
        self.gpu = Some(gpu::Gpu::new(&window));
        self.fonts = Some(fonts);
        self.app = Some(app);
        window.request_redraw();
        self.window = Some(window);
        #[cfg(target_os = "macos")]
        macmenu::install(self.proxy.clone());
        #[cfg(target_os = "macos")]
        notify::install(self.proxy.clone());
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        // A time-based state change (working → idle, flash expiry) is due.
        if let (StartCause::ResumeTimeReached { .. }, Some(app)) = (cause, self.app.as_mut()) {
            app.dirty = true;
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, ev: Ev) {
        if let Some(app) = self.app.as_mut() {
            app.handle(ev);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let Some(app) = self.app.as_mut() else { return };
        match ev {
            WindowEvent::CloseRequested => self.shutdown(el),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(s) => {
                app.w = s.width as f32;
                app.h = s.height as f32;
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(s.width, s.height);
                }
                app.dirty = true;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                app.scale = scale_factor as f32;
                if let Some(f) = self.fonts.as_mut() {
                    f.scale = app.scale;
                    f.reset();
                }
                app.dirty = true;
            }
            WindowEvent::Occluded(o) => {
                self.occluded = o;
                app.dirty |= !o;
            }
            WindowEvent::Focused(f) => app.window_focused = f,
            WindowEvent::ModifiersChanged(m) => app.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => app.on_key(&event),
            WindowEvent::CursorMoved { position, .. } => app.on_cursor_moved(position.x as f32, position.y as f32),
            WindowEvent::MouseInput { state, button, .. } => app.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 3.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / app.cell_h,
                };
                app.on_wheel(lines);
            }
            WindowEvent::DroppedFile(path) => app.on_paste(&format!("{} ", path.display())),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(app) = self.app.as_mut() else { return };
        if app.quit {
            return self.shutdown(el);
        }
        let Some(w) = self.window.as_ref() else { return };
        app.check_attention();
        if std::mem::take(&mut app.raise) {
            w.focus_window();
        }
        if app.dirty {
            app.sync_layout();
            // Hidden: stay dirty, so becoming visible draws. A `pmux snapshot` still gets its frame.
            if !self.occluded || app.snapshot.is_some() {
                app.dirty = false;
                w.set_cursor(app.cursor_icon);
                w.request_redraw();
            }
        }
        // Sleep until the next event, or a time-based state change. No idle ticks.
        el.set_control_flow(match app.next_deadline() {
            Some(d) if d > Instant::now() => ControlFlow::WaitUntil(d),
            Some(_) => ControlFlow::Poll,
            None => ControlFlow::Wait,
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run(),
        Some("-h" | "--help" | "help") => print!("{USAGE}"),
        Some("guide") => print!("{}", api::GUIDE),
        Some("hold") => std::process::exit(hold::run(&args[1..])),
        Some("import") => std::process::exit(import::main(&args[1..])),
        Some("hook") => std::process::exit(hook::run(&args[1..])),
        // Debugging agent manifests: `pmux detect claude [title] < screen.txt`
        Some("detect") => std::process::exit(detect::cli(&args[1..])),
        Some("integration") => std::process::exit(hook::integration(&args[1..])),
        Some(_) => std::process::exit(api::client(args)),
    }
}

fn run() {
    let el = EventLoop::<Ev>::with_user_event().build().expect("event loop");
    let proxy = el.create_proxy();
    let socket = match api::listen(proxy.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pmux: socket: {e}");
            std::process::exit(1);
        }
    };
    let mut r = Runner {
        proxy,
        socket,
        app: None,
        window: None,
        gpu: None,
        fonts: None,
        bufs: Default::default(),
        occluded: false,
    };
    let _ = el.run_app(&mut r);
}
