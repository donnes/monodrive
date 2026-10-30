# Worktrees

`monodrive-core`'s `worktrees` reads a Git repository's worktrees and one worktree's changes by running git. Git stays the authority: Monodrive keeps no copy of what `git worktree list` says, only the repositories the user or an agent registered. Nothing here fetches, takes Git's locks, runs hooks, starts the index or walks the disk.

## Which git

`Git::find` takes the first `git` on `PATH`, then `/opt/homebrew/bin/git`, then `/usr/local/bin/git`. The window, opened from the Dock, gets launchd's `PATH`, which has only `/usr/bin`, so the fixed folders let it find the same git as an MCP server started from a terminal. `/usr/bin/git` is skipped during that search and comes last, only when `xcode-select -p` names a developer folder holding git. Without Xcode or the Command Line Tools it isn't git but a shim that opens the installer, so Monodrive reports that Git isn't installed instead of running it. Listing needs git 2.36 or later, for `worktree list -z`; an older git's error passes through.

## How git runs

Each command gets an argument vector, never a shell, and an environment of Monodrive's own: `HOME`, `XDG_CONFIG_HOME` and `DEVELOPER_DIR` from Monodrive's, so the user's global config and ignores apply, and nothing else, so a `GIT_DIR` from the caller can't point git at another repository. On top of that, `GIT_OPTIONAL_LOCKS=0`, so `git status` reads the index without refreshing it or taking `index.lock` under another git, `GIT_TERMINAL_PROMPT=0`, `LANG=C`, `--no-pager` and `core.fsmonitor=false`, so a status never starts a monitor daemon in the user's repository.

A command gets 10 seconds by default, and `worktree_status` takes `timeout_seconds`. Monodrive reads up to 8 MB of output and 64 KB of errors. When git runs out of time or prints more, Monodrive kills the git it started, only that process, and reaps it. A thread waits for the child with `waitid(WNOWAIT)`, which leaves it unreaped, so its pid can't go to another process before the kill.

## Repositories and states

A repository is known by its common Git folder, through `rev-parse --git-common-dir`, past any links. The repository, a folder inside it, a link to it, its `.git` folder and each of its worktrees all resolve to the same one, so registering or listing through any of them gives one repository. A registration saves that folder and the main worktree's path, or the bare repository's, to show. Loading registrations runs no git: one whose common folder is gone or lost its `HEAD` is unavailable, not an error. Forgetting one removes only the bookmark.

`git worktree list --porcelain -z` gives each worktree's path, HEAD, branch or detached, bare, and Git's locked and prunable notes, with their reasons. Its first entry is the main worktree, or the bare repository. Each worktree's folder is then checked: missing when it's gone, perhaps on a drive that isn't connected; stale when it's there but its `.git` isn't; unreadable when it can't be looked into. Prunable is what Git would prune, and never means a folder is safe to delete: Git calls a worktree it can't look into prunable, and a locked worktree on an unplugged drive is missing but not prunable.

Status runs `git status --porcelain=v2 -z --branch --untracked-files=normal` for one worktree, on demand, and counts staged, unstaged, conflicted and untracked paths. Ahead and behind come from `branch.ab`, which git prints only when the upstream's ref is already here, so they're unknown until it's fetched, and Monodrive never fetches.
