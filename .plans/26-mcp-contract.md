# MCP contract v1

Status: done. `docs/mcp.md` states the contract and `crates/monodrive-core/contract/v1.json` holds it. Reviewed on 2026-11-09: the maintainer picked two profiles (core and all), verb_noun names for management tools, `id:/path` for servers with ZIPs listed as folders, and scans that reply when done or at their timeout with `partial: true`.

## The problem

- **Costly to list.** `tools/list` returned 69 tools in 32 KB, about 8,000 tokens, per agent session before any work happens, when this plan was written. Dropping comparison and catalogs took it to 61 tools.
- **Replies are prose.** Every result is text, with no `outputSchema` or `structuredContent`, so a wording change can break a client.
- **Schemas can drift.** `ToolCall` and the JSON schemas in `TOOLS` are written by hand, in parallel, in a 5,431-line file.
- **Implementation shows through.** `remote_list`, `remote_stat` and `remote_copy` duplicate `list_dir`, `stat` and `copy` because the backend differs. Previews have three shapes: `rename_preview`, `cleanup_duplicates` and `command_preview`.
- **Errors are text.** An agent can't tell "retry" from "give up". Bad arguments come back as protocol error `-32602`, while the 2025-11-25 spec makes input validation a tool execution error (`isError: true`), which is what lets a model correct itself.

## What the contract promises

Tools state outcomes, never mechanisms. An agent asks for a copy. Whether that's `clonefile`, `copyfile` or chunks can change, and the reply only reports what happened.

**Stable names and meanings.** A published tool, field or error code keeps its name and meaning. Changing a meaning needs a new name. Text wording and descriptions aren't part of the contract.

**Allowed without a new version:** new tools, new optional inputs, new output fields, new accepted input values, new error codes and job states. Clients must tolerate unknown output fields and values, so output schemas stay open and never set `additionalProperties: false`. Input schemas do set it, so a misspelled argument gets a clear error instead of silently doing the default.

**Breaking:** removing or renaming anything, a new required input, a changed type or default, a removed input value. A break ships as a new tool name beside the old one for at least one stable release. The old tool is marked deprecated in its description and in `tools/list`.

**Version.** `initialize` reports `capabilities.experimental["monodrive/contract"] = { "version": 1 }`, separate from the app version. It stays `0` until frozen, and `0` may break freely.

## Shapes every tool shares

**Results.** `content` keeps the terse text AGENTS.md asks for: paths, one per line. `structuredContent` carries the typed result, described by `outputSchema`. The spec says to also put serialized JSON in text for old clients. We won't, because that doubles the tokens and the terse text already stands alone. **Check before freezing:** which of `content` and `structuredContent` the model actually sees in Claude Code, Codex and Cursor, and whether any sends both. If one client shows only `structuredContent` or sends both, the reply shape changes before v1.

**Errors.** `isError: true` with `{ "code", "message", "retryable", "path"? }`. Protocol errors remain only for unknown tools and malformed requests. Fsearch's strings pass through in `message`, as AGENTS.md requires. First codes: `invalid_argument`, `not_found`, `permission_denied`, `name_taken`, `changed_since_preview`, `no_trash_on_volume`, `not_downloaded`, `unsupported_on_source`, `agents_disabled`, `index_unavailable`, `busy`, `internal`.

**Long-running work.** Every such tool returns the same `job` object, whose `state` may already be `completed`. An optional `wait_seconds` (default 0, capped) holds the reply until it finishes or the wait runs out. This is what makes the contract survive implementation changes: a copy that takes 100 ms or 10 minutes has one reply shape, and an agent's code doesn't change when the copy gets faster. Don't build on MCP's task-augmented execution yet: it's experimental in the 2025-11-25 spec. `execution.taskSupport` can map onto these jobs later without changing the tools.

**Previews.** One pattern everywhere: `dry_run: true` returns what would happen plus a `token`; calling again with `confirm: token` runs exactly that plan, once. This replaces `rename_preview`, `command_preview` and the two-step form of `cleanup_duplicates`.

**Retries.** Every changing tool takes an optional `request_id`. The owner already answers a repeated id with its recorded outcome (`docs/operations.md`), so this exposes existing behavior. A lost reply can't cause a double move.

**Lists.** Anything that can be long takes `limit` and returns `next_cursor` and `truncated`. Cursors are opaque. `list_dir` on 100,000 entries must not return 100,000 lines by default.

**Sources.** Tools that read or change files take an optional `source`: omitted means this Mac, or `remote:<id>`, `archive:<path>`. A source that can't do something answers `unsupported_on_source`. This follows the plans README rule to check capabilities rather than promise them.

**Annotations.** Every tool sets `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint`. They're advice for clients. The owner stays the enforcement point, and `agents_change_files` stays as it is.

**Speed fields.** `copy` and `duplicate` report `method: "clone" | "copy"` per item, and take `require_clone: true` to fail rather than write a full copy. These report outcomes and promise no speed.

## Tool map

69 tools became 54. Core has 29; `all` adds 25.

| v1 tool | Replaces | Profile |
|---|---|---|
| `find_files`, `grep`, `thumbnail`, `index_status` | themselves | core |
| `list_dir` | itself, `remote_list`, `archive_list` | core |
| `stat` | itself, `remote_stat` | core |
| `read_file` | `remote_read`, and local files | core |
| `folder_size`, `overview`, `storage_scan`, `duplicates`, `cleanup_duplicates` | themselves | core |
| `trash`, `put_back`, `undo`, `redo`, `duplicate`, `create_file`, `move` | themselves | core |
| `rename` | itself, `rename_preview`, `rename_many`, `remote_rename` | core |
| `create_folder` | itself, `create_folder_with`, `remote_mkdir` | core |
| `copy` | itself, `remote_copy` | core |
| `archive_create`, `archive_extract`, `download`, `remove_download` | themselves | core |
| `job_status`, `cancel_job` | themselves, `command_status`, `command_cancel` | core |
| `continue_job` | itself | core |
| `edit_image`, `edit_video`, `organize`, `suggest_names` | themselves | all |
| `list_places` | itself, `remote_sources` | all |
| `pin`, `unpin`, `eject`, `connect` | themselves | all |
| `disconnect`, `forget_server` | themselves, `remote_disconnect`, `remote_forget` | all |
| `add_server` | `remote_add` | all |
| `list_searches`, `save_search`, `rename_search`, `remove_search` | themselves | all |
| `list_worktrees`, `list_repos`, `register_repo`, `forget_repo` | `worktrees`, `repos`, `repo_register`, `repo_forget` | all |
| `remove_worktree` | `worktree_remove` | all |
| `worktree_status` | itself | all |
| `list_commands`, `run_command` | `commands_list`, `command_run`, `command_preview` | all |
| `check_updates` | `update_check` | all |

`monodrive mcp --profile all`, or Management tools on Settings' Agents page, offers the second profile. A test keeps what core gives a model under 16 KB.

## How it's built

Split `mcp.rs` into `mcp/contract.rs` (argument and result types, error codes), `mcp/server.rs` (protocol) and `mcp/tools/*.rs` (each tool decodes, calls one service, formats). This matches AGENTS.md's "decode into a typed enum, call one service, terse text reply".

Generate schemas from the types with `schemars`, so a schema can't disagree with what's decoded. GPUI already pulled it in, under MIT. Output types are the contract's own, converted from service types. Inputs reuse the services' argument types where they already were the tools' (`rename::Rule`, `edits::Step`, `Conflict`), since the snapshot test catches a field renamed underneath.

## Keeping it stable

- **Snapshot.** `crates/monodrive-core/contract/v1.json` holds names, schemas, annotations and error codes. A test compares it with the live list and applies the rules above: additions pass and refresh the file, and breaks fail with the rule they broke.
- **Black-box tests.** Contract tests drive `handle` with JSON-RPC over temp folders and only look at replies, never at services. They replace most of today's text-matching tests in `mcp.rs` where those check structure. If the copy path changes and these pass, the contract held.
- **Docs.** `docs/mcp.md` states the promises above in a page, since they span components and the code can't show them. Add a contract check to the AGENTS.md "Hit every surface" list.

## Order

1. Split the file, generate schemas, and add the error model, with today's tools. No behavior change.
2. Add structured results, the job shape, previews and cursors, tool group by tool group.
3. Merge sources and apply the tool map.
4. Try it with Claude Code, Codex and Cursor on real tasks. Record what the model saw, how many tokens it cost, and which tool it wrongly avoided.
5. Freeze: contract version 1, snapshot, and `docs/mcp.md`. Update `agents.rs` and `INSTRUCTIONS`.

Each step is its own PR. Before step 5, breaks are free.

## Trial

Step 4 ran on 2026-11-09 with a dev build over fixtures in `.scratch/`, one prompt with four tasks: what a ZIP holds, trash a duplicate and undo, rename photos after a dry run, copy a file and say whether it cloned.

- **Claude Code** (2.1.319) finished all four with Monodrive's tools alone and used no shell. It saw `structuredContent`, not the text, so `list_dir`'s structured reply dropped per-item paths and dates. The rename dry run caught a double space in its first rule before anything changed. The whole session took about 33,000 tokens to cache and $0.39.
- **Codex** (0.167.0) finished all four once Monodrive's tools were pre-approved; without that, `codex exec` refused every tool not marked read-only, the `rename` dry run included. It ran `rg --files` once where `list_dir` would do. It repeats the server instructions after every description, so they were cut to four lines. 282,000 input tokens, 237,000 of them cached.
- **Cursor**'s CLI wasn't signed in, so it wasn't tried.

## Not in this plan

No HTTP transport, no resources or prompts, no change to who may empty the Trash, and no faster implementations. Those are in [plan 25](25-filesystem-speed.md), and this plan's job is that they don't change anything here.
