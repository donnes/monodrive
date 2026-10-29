# Saved browsing workspaces

Depends on tabs, split panes and source identity. Independent of Git worktrees and optional storage backends.

## Scope

Save a named browsing arrangement: tabs, paired locations, view choices and search filters. Restore it, rename it, update it and forget it through commands and the UI. Persist a small versioned layout through shared core services, with MCP equivalents for named workspace management and restoration requests.

A "Photo import" workspace can restore an external drive beside Pictures. A missing source stays visibly unavailable and offers reconnect or another location; it must not resolve to a different drive using the old mount path. Saving or restoring does not transfer files, scan sources or launch agents.

Reuse relevant Monocode pane state handling. Start with the existing two-pane model. Do not add arbitrary layouts or per-folder `.DS_Store` persistence.

## Verification

Test saved state round trips, missing sources, invalid old state and forget-without-file-changes. Verify keyboard focus and restored layouts through headless UI checks.
