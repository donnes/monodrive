# User commands

A user command is a program the user saved to run on a selection. `user_commands` holds definitions, previews and runs. Agents and the window share them through the operation owner.

## Definitions

Definitions live in `places.json`, under the same flock and atomic rename as the favorites. `Store::save_command` validates one and replaces any saved under its name.

- **Program.** An absolute path, or a name looked up when it runs on `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`, or on the definition's own `PATH`.
- **Arguments.** A whole argument `{paths}` becomes every selected path, each its own argument, in one invocation. A whole argument `{path}` runs the command once per path, one at a time. The two can't mix, and neither can sit inside a longer argument, so a name is never joined into a string. Without either, the command runs once and ignores the paths.
- **Folder.** `selection` is the folder holding the items, or with `{path}`, each item's own. `{paths}` over several folders is refused, since no one folder holds them. `home` and `folder` are fixed.
- **Sources.** `files`, `folders` or `both`, following links to decide. Remote sources are refused: `remote` doesn't decode, and a path that isn't absolute, such as `sftp://host/a`, fails the preview, so a remote path is never read as a local one.
- **Environment.** Additions are stored in plain text in `places.json`. A name containing `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `API_KEY` or `PRIVATE_KEY` is refused. That catches mistakes and doesn't detect secrets; a secret belongs where the program reads its own.
- **Limits.** A timeout of 1 second to 24 hours for each invocation, 60 seconds by default. An output limit of 1 KB to 1 MB for the run, 64 KB by default. A run takes at most 1,000 paths.

## What a run passes

Nothing goes through a shell string. The program gets an argument vector, and the selection's paths arrive exactly as given.

Paths must be absolute, so each starts with `/` and no program can read a file named `-rf` as an option. That's why Monodrive inserts no `--` and prefixes no `./`. A definition that wants `--` writes it in its arguments.

A shell or script is allowed as the program, and paths still arrive as `$1` and on: `/bin/sh -c 'for f; do echo "$f"; done' sh {paths}`. Saving refuses a placeholder right after a flag that runs its next argument as code, such as `sh -c {path}`, `bash -lc {path}`, `python3 -c {path}`, `perl -e`, `node -e` or `osascript -e`. This check knows those interpreters by name, so it stops the common mistake and can't stop every one: `/usr/bin/env sh -c {path}` passes it.

Each invocation gets an environment of Monodrive's own: `HOME`, `USER`, `LOGNAME` and `TMPDIR` from the owner, `PATH` as above and `LANG=en_US.UTF-8`, then the definition's additions. The owner may have been started by the window or by an agent's server, so inheriting its environment would make a command behave differently depending on which came first. Standard input is empty.

`preview` returns each invocation's exact arguments and folder, reading the disk and running nothing. The owner works the plan out again from what's saved when it runs, and the run keeps that plan, so a client can always see what really started.

## Runs and the owner

The owner runs commands, so the window and every MCP server see and cancel the same runs, and a run outlives the agent session that started it. The owner stays until its runs end, as it does for jobs.

Runs aren't owner jobs. A job is a journaled file operation with items, destinations, conflict choices and an undo entry, and the window's popover offers Retry and Replace for each. A command has none of these, so runs are a small registry beside the jobs, with no journal. Their ids are their own and restart with the owner, which forgets ended runs when it exits. The window hears jobs through the journal, so the window will need to ask for runs some other way once it shows them.

The owner keeps every run going and the newest 20 ended, and refuses a ninth at once. A client resending a request after a lost connection gets the same run, unless the owner itself died, in which case the command can start twice. A crash leaves the command running in its own process group, since macOS has no way to tie a child's life to its parent's.

## Stopping

Each invocation is the leader of a process group Monodrive made for it, and only that group is ever signalled, never a name or a pattern. A thread waits for the leader with `waitid(WNOWAIT)`, which leaves it unreaped, so its pid and group id can't go to another process before the last signal.

Cancel and the timeout send SIGTERM to the group, then SIGKILL 2 seconds later unless the leader has exited and the output has closed. Cancel stops the invocations still to come; after a timeout or a failure the rest still run. When the leader exits by itself, anything still holding its output gets the same SIGTERM and SIGKILL, and anything else left in the group gets SIGKILL, so a command never leaves processes Monodrive can't show or cancel. A command that wants something to outlive it must start it outside its group, such as with `open`. A process that leaves the group itself is out of Monodrive's reach, and if it keeps the output open, the run stops waiting for it 2 seconds after the leader is gone.

## Output and results

Standard output and errors share one pipe, so they stay in order. A run keeps its first and last bytes up to the output limit, half each, across all its invocations in order, and counts what it left out. The rest is read and dropped, so a flood never blocks the command or fills memory. Text is decoded leniently for display.

Each invocation reports exit code, signal, timed out, cancelled or why it couldn't start, and how long it took. A run completes when every invocation exited 0, is cancelled when cancelled, and fails otherwise.

Nothing a command changes can be undone. Monodrive can't see what it did, so runs join no undo history, and MCP's replies say so.

## Agents

Agents run saved definitions only: `command_run` sends a name, and the owner reads the definition itself. No MCP tool saves one, since an agent that could save a command could run anything; saving stays with the user, through `Store::save_command`. Agents run commands only while `agents_change_files` is on, read at each request, because a command can change files. `commands_list` and `command_preview` run nothing and stay open, and `command_cancel` is always allowed. Like the setting itself, this controls a feature and isn't a security boundary.
