# pmux

An agent-aware terminal multiplexer for macOS, written in Rust. A GPU-rendered window with a sidebar of
workspaces, projects and git worktrees, where every pane can list, read and message the others, and reports its
state (working, waiting, idle, done) so you can see which agent needs you.

Terminals live in a background `pmux hold` process, so closing, crashing or rebuilding the window never kills
them.

## Install

Download `pmux-<version>-macos-arm64.zip` from the releases page, unzip it, and move `pmux.app` to
`/Applications`. The app is ad-hoc signed, not notarized, so macOS blocks the first launch. Clear the quarantine
flag once:

```sh
xattr -dr com.apple.quarantine /Applications/pmux.app
```

To use the CLI from your shell, link the binary into your `PATH`:

```sh
ln -s /Applications/pmux.app/Contents/MacOS/pmux /usr/local/bin/pmux
```

## Build from source

Needs Rust (stable) and Xcode command line tools (for `swift`, `sips`, `iconutil`, `codesign`).

```sh
tools/install-app.sh          # build and install /Applications/pmux.app
tools/dist.sh                 # build dist/pmux-<version>-macos-<arch>.zip to share
```

## Usage

```
pmux                      open the pmux window
pmux ls                   list panes
pmux tree                 workspaces / projects / worktrees / panes
pmux read <pane> [n]      last n lines of a pane's screen
pmux send <pane> <text>   type text + Enter into a pane
pmux new [cmd...]         open a pane next to this one
pmux status <state>       set my state: working|waiting|idle|done|error|clear
pmux guide                how agents should use pmux
pmux help                 everything else, plus key bindings
```

Run `pmux integration install claude` to let Claude Code report its session, so it resumes after restarts.

## Credits

- Fonts: [Lilex](https://github.com/mishamyrt/Lilex) (OFL) and [IBM Plex Sans](https://github.com/IBM/plex)
  (OFL); licenses in `assets/fonts/`.
- File icons are generated from [Zed](https://github.com/zed-industries/zed)'s file icon set
  (`tools/zed_icons.py`).
- Ideas from c11, cmux, tmux and Herdr.

## License

GPL-3.0-or-later. See `LICENSE`.
