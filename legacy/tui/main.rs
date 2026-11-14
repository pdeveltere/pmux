mod api;
mod app;
mod config;
mod git;
mod keys;
mod pty;
mod render;
mod watch;

const USAGE: &str = "pmux: agent-aware terminal multiplexer (prototype)

usage:
  pmux                      open pmux in this terminal
  pmux ls                   list panes
  pmux tree                 workspaces / projects / worktrees / panes
  pmux whoami               this pane
  pmux read <pane> [n]      last n lines of a pane's screen
  pmux send <pane> <text>   type text + Enter into a pane
  pmux new [cmd...]         open a pane next to this one (optionally run cmd)
  pmux status <state>       set my state: working|waiting|idle|done|error|clear
  pmux title <text>         set my title
  pmux guide                how agents should use pmux

panes: %N, N, self, or a title

keys (prefix C-a):
  Tab sidebar   | new pane   h/l focus   x close   j/k prev/next worktree
  c clear pane   r redraw screen   n next pane needing you   b hide sidebar
  q quit   C-a C-a literal C-a
sidebar:
  j/k move  Enter open  h/l fold  a add project  w new worktree  W new workspace
  D remove worktree  x remove project  Esc back
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            if let Err(e) = app::run() {
                eprintln!("pmux: {e}");
                std::process::exit(1);
            }
        }
        Some("-h" | "--help" | "help") => print!("{USAGE}"),
        Some("guide") => print!("{}", api::GUIDE),
        Some(_) => std::process::exit(api::client(args)),
    }
}
