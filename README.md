# Monodrive

A fast file manager for macOS with the look of Monocode, built with [GPUI](https://www.gpui.rs) and searching the whole disk through [fsearch](https://github.com/noahdunnagan/fsearch). It also runs as an MCP server, so coding agents search your files through the same index instead of Spotlight.

Early scaffold: the window browses folders and searches by name; the MCP server offers `find_files`, `grep` and `list_dir`.

## Run

```sh
cargo run -p monodrive              # the window, on your home folder
cargo run -p monodrive -- ~/Work    # another folder
cargo run -p monodrive -- mcp       # the MCP server on stdio
```

The search field has focus when the window opens, and ⌘F returns to it. ↑ and ↓ move the selection, Enter opens it, Escape clears the search, and ⌘[ / ⌘] / ⌘↑ go back, forward and up.

To use it from Claude Code:

```sh
cargo build -p monodrive
claude mcp add monodrive -- "$PWD/target/debug/monodrive" mcp
```

The first search on a Mac without an fsearch index crawls the whole disk in the background, which takes a few minutes. The index lives in `~/Library/Application Support/FSearch` and is shared with the fsearch CLI. Grant Full Disk Access to the binary to index protected folders.

## Develop

`scripts/check` runs the same checks as CI. See [AGENTS.md](AGENTS.md) for how we work.

Monodrive is MIT licensed; see [LICENSE](LICENSE). Credits for the code and assets it builds on are in [NOTICE](NOTICE).
