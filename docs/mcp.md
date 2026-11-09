# The MCP contract

`monodrive mcp` is how coding agents use Monodrive. Agents and the scripts around them depend on its tools, so the tools are a contract with a version, apart from the app's. This page says what the contract promises. `crates/monodrive-core/contract/v1.json` holds it, and a test compares that file with the live tools on every run.

## Profiles

`tools/list` offers one of two profiles. **core** is the default: finding, reading and changing files, the jobs that do it, and what takes up space, 29 tools. **all** adds places, servers, saved searches, Git, saved commands, edits and organize, 54 tools. `monodrive mcp --profile all` picks it on the command line; without the flag, the server reads Settings' Agents page ("Management tools") when the session starts. A tool outside the profile answers like a tool that doesn't exist.

Profiles exist because every listed tool costs each agent session tokens before it does anything. A test keeps what core gives a model (names, descriptions and input schemas) under 16 KB.

## What stays stable

A published tool's name, profile, input and output fields, their meanings, and the error codes keep their names and meanings. Text in `content` and descriptions aren't part of the contract: they're for a model to read and may be reworded.

Allowed without a new version: new tools, new optional inputs, new output fields, new accepted input values, new error codes, new job states, and moving a tool into core.

Breaking: removing or renaming a tool, field or code; a new required input; a changed type or default; a removed input value; moving a tool out of core. A break ships as a new tool beside the old one, which stays for at least one stable release with "Deprecated:" leading its description. When there's no other way, the contract's version goes up.

The contract test applies these rules. An addition refreshes `contract/v1.json`, which goes in the same commit; a break fails the test and names the rule.

## Shapes every tool shares

**Input.** Input schemas set `additionalProperties: false`, so a misspelled argument fails instead of quietly doing the default. Arguments that don't decode are a tool error with `invalid_argument`, not a protocol error, so the model sees it and can correct itself. Protocol errors are left for unknown tools and malformed requests.

**Results.** `content` holds terse text: paths, one per line. `structuredContent` holds the same as typed data, described by the tool's `outputSchema`. Output schemas are open: clients must accept fields and values they don't know. Neither has a required field at the top, since a failed call's result holds only `error`.

**Errors.** A failed call sets `isError: true`. Its text ends with `(error: code)`, and `structuredContent.error` holds `{code, message, retryable, path?}`. A call that changes several items fails only when none changed; otherwise each item says how it went. Messages from fsearch and the services pass through unchanged.

| Code | Meaning |
|---|---|
| `invalid_argument` | Fix the arguments. |
| `not_found` | No such file, job, server or saved item. |
| `permission_denied` | macOS or the server refused. |
| `name_taken` | Something already has that name. |
| `changed_since_preview` | The files or plan changed, or the token is spent or expired. Dry run again. |
| `no_trash_on_volume` | The volume can't move this to a Trash. |
| `not_downloaded` | A cloud placeholder; `download` it first. |
| `unsupported_on_source` | This tool can't do that on a server or inside a ZIP. |
| `clone_unavailable` | `require_clone` was set and the copy would write every byte. |
| `agents_disabled` | Let agents change files is off. |
| `index_unavailable` | The index couldn't start. Retryable. |
| `auth_required` | The server needs a password or a host key Monodrive won't prompt for. |
| `unreachable` | The server didn't answer. Retryable. |
| `busy` | Something else holds it. Retryable. |
| `failed` | The disk or a service said no, for the reason in the message. |
| `internal` | A bug in Monodrive. |

**Jobs.** Copies, moves, archives, continued jobs and saved commands reply with one job object, even when it's already done. `wait_seconds`, up to 60, holds the reply until a file job ends; the server waits on the owner's journal, not a clock. Job ids are opaque strings; a saved command's starts with `cmd-`. A copy that takes 100 ms or 10 minutes has the same reply, so an agent's code doesn't change when Monodrive gets faster.

**Dry runs.** `rename`, `cleanup_duplicates`, `organize`, `remove_worktree` and `run_command` take `dry_run: true`, which returns the plan and a `token`. `confirm: token` runs exactly that plan, once, within 10 minutes, and the owner refuses it whole if the files changed since. `cleanup_duplicates` and `organize` only run a confirmed plan, and `remove_worktree` needs one when the worktree holds work nothing else has.

**Retries.** Every tool that changes files through the owner takes `request_id`. Sending the same id again, from any session, returns the first outcome instead of changing anything twice (`docs/operations.md`).

**Lists.** Anything that can be long takes `limit` and returns `truncated` and, when there's more, `next_cursor`. Cursors are opaque.

**Paths.** Local paths are absolute or start with `~`. A saved SSH server's are `id:/path`, and `id:` alone is where it starts browsing. A local path through a `.zip` file is inside it, for `list_dir` only. A tool that can't work on a server or inside an archive says `unsupported_on_source`.

**Hints.** Every tool sets `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint`. They're advice for clients; the operation owner enforces the Agents setting.

**Copies.** `copy` and `duplicate` report `method: clone | copy` per item, and `require_clone: true` refuses before anything is written when a copy can't share blocks. These report outcomes and promise no speed.

## What clients do with it

Tried on 2026-11-09 with the same four tasks in each: look inside a ZIP, trash a duplicate and undo it, rename photos after a dry run, and copy a file and say whether it cloned.

- **Claude Code** gives the model `structuredContent` and drops the text. So structured results carry what the text does and no more: `list_dir` returns names under the folder's path, not a path and a date per item. It defers MCP tools behind its tool search, so a long `tools/list` costs little there.
- **Codex** calls tools from a script that sees the whole result, text and `structuredContent` both. It repeats the server's instructions after every tool's description, so they stay to a few lines. It asks before any tool without `readOnlyHint`, dry runs included; `codex exec` can't ask, so it needs `default_tools_approval_mode = "approve"` for the server.
- **Cursor**'s CLI wasn't tried: it wasn't signed in.

**Version.** `initialize` reports `capabilities.experimental["monodrive/contract"] = {"version": 1}`.
