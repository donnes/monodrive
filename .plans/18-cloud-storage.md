# Cloud storage

Local cloud-provider folders only. Depends on source capabilities, operations and preview materialization. Direct provider connections, such as S3, were dropped on 2026-10-30: nothing needs them.

## Local cloud folders

Recognize when a visible file is only a placeholder if native provider metadata exposes that state. Do not hydrate a directory just to list or calculate its size. Opening, previewing or copying may request a download; show that work and errors explicitly. Report unknown availability when the provider does not expose it rather than guessing.

Use native provider interfaces for supported download/availability operations. Do not manipulate provider internals or promise uniform behavior across every installed service. MCP observes the same availability and download results as the UI.

## Verification

Use controlled placeholders or provider mocks for hydration behavior. Cover interruption and permission errors.
