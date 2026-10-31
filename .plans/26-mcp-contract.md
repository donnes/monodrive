# MCP contract v1

Status: planned; tool map and profiles need a maintainer's review. Nobody uses the MCP server yet, so this is the one cheap moment to break it. It should land before the MCP server is promoted in a stable release.

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

## Proposed tool map

Keep the behavior; change the names and shapes. Reviewed against `TOOLS` on 2026-10-30.

| v1 tool | Replaces |
|---|---|
| `find_files` | itself |
| `grep` | itself |
| `list_dir` | itself, `remote_list`, `archive_list` |
| `stat` | itself, `remote_stat` |
| `read_file` | `remote_read` |
| `folder_size`, `index_status` | themselves |
| `trash`, `put_back`, `undo`, `redo`, `duplicate` | themselves |
| `rename` | itself, `rename_preview`, `rename_many`, `remote_rename` |
| `create_folder` | itself, `create_folder_with`, `remote_mkdir` |
| `create_file` | itself |
| `copy` | itself, `remote_copy` |
| `move` | itself |
| `archive_create`, `archive_extract` | themselves |
| `storage_scan`, `duplicates`, `cleanup_duplicates` | themselves |
| `job_status`, `cancel_job`, `continue_job` | themselves, `command_status`, `command_cancel` |

That's 25 tools in the core profile.

**Management tools** stay available in an `all` profile, chosen with `monodrive mcp --profile all` and on Settings' Agents page: places (`list_places`, `pin`, `unpin`, `eject`), servers (`connect`, `disconnect`, `forget_server`, `remote_sources`, `remote_add`, `remote_forget`, `remote_disconnect`), downloads, saved searches, repos and worktrees, and user commands. An agent that needs these can be configured for them, so principle 2 in AGENTS.md still holds. **This is the decision to review:** the alternative is listing everything always and paying about 8,000 tokens, or folding each group into one tool with an `action` field, which gives weaker schemas. A test keeps the core profile's `tools/list` under 16 KB.

## How it's built

Split `mcp.rs` into `mcp/contract.rs` (argument and result types, error codes), `mcp/server.rs` (protocol) and `mcp/tools/*.rs` (each tool decodes, calls one service, formats). This matches AGENTS.md's "decode into a typed enum, call one service, terse text reply".

Generate schemas from the types with `schemars`, so a schema can't disagree with what's decoded. It's a new dependency, so check its license against `deny.toml`. Types stay independent of service types, with a conversion between them, so renaming a core field can't change the contract by accident.

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

## Not in this plan

No HTTP transport, no resources or prompts, no change to who may empty the Trash, and no faster implementations. Those are in [plan 25](25-filesystem-speed.md), and this plan's job is that they don't change anything here.
