# User commands and extensions

Parked on 2026-10-30: the core is on main with no UI, and nothing more gets added until a real workflow needs it. A broad extension runtime was dropped. Depends on settled command and operation services, and isn't a prerequisite for remote locations or agent handoff.

## User commands first

Allow explicitly configured commands on a selection through the palette and context menu. Define executable, argument vector, working directory and supported source type. Pass filenames as arguments, never interpolate them into a shell string. A user may explicitly choose a shell script, but selected paths still need safe argument passing.

Show what will run and which machine runs it. Local commands cannot treat remote paths as local ones. Keep credentials out of saved command definitions, bound output retention and track only processes Monodrive starts. Cancellation must not kill by name or affect unrelated processes.

Commands share job visibility and MCP invocation of the same saved definitions. External scripts can perform changes outside Monodrive's operation service, so do not promise undo or silently treat arbitrary scripts as reversible file operations. Configuration and execution are explicit user choices.

## Verification

Use harmless controlled executables to test unusual filenames, argument boundaries, cwd, source restrictions, cancellation and output limits. Never run a user's configured scripts during tests.
