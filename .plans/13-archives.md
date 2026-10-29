# Archives

Depends on operations, progress/cancellation and explicit conflict handling.

## Scope

Start with creating ZIP files, browsing archive entries read-only, and extracting selected entries or the full archive to an explicit destination. Add other formats only with a demonstrated need and an approved dependency. Opening an archive does not extract it or execute its contents.

Extraction and compression are shared core jobs with UI commands and MCP equivalents. Preview destination conflicts and use the existing policies. Keep the input archive unchanged. Undo trashes completed output through the operation service; interrupted output remains identified and recoverable.

Treat archive paths as untrusted. Reject traversal outside the chosen destination, absolute paths and link-based escapes. Apply bounded extraction limits and cancellation for unexpectedly large output. Do not follow extracted links while writing later entries. Report unsupported encryption or formats directly; no silent fallback to shell commands.

Archive browsing has Back, and cancelling a job reports completed and partial outputs. Use bounded background work and inspect metadata before allocating large buffers.

## Verification

Use small local archive fixtures for round trips, conflicts, traversal/link escapes, size limits and cancellation. Test the real extraction boundary. Check responsiveness on an explicitly generated large archive without scanning the user's folders.
