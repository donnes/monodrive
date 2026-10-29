# Cloud storage

Two slices: local cloud-provider folders, then optional direct provider connections. Depends on source capabilities, operations and preview materialization. Direct providers also depend on the remote connection/job model.

## Local cloud folders

Recognize when a visible file is only a placeholder if native provider metadata exposes that state. Do not hydrate a directory just to list or calculate its size. Opening, previewing or copying may request a download; show that work and errors explicitly. Report unknown availability when the provider does not expose it rather than guessing.

Use native provider interfaces for supported download/availability operations. Do not manipulate provider internals or promise uniform behavior across every installed service. MCP observes the same availability and download results as the UI.

## Direct providers, only when needed

Choose one concrete provider use case before implementation, such as S3-compatible storage. Reuse source-qualified paths, capability checks, credentials handling and transfer jobs; do not add a dozen SDKs speculatively.

A bucket prefix is not a POSIX directory. Define pagination, read/write support, versioning, rename-by-copy semantics, conflict handling and recovery before exposing mutations. Unsupported reversible removal remains unavailable. Credentials stay in the OS credential store and never appear in plans, preferences or tool output.

Connecting does not enable synchronization or catalog the entire account. Disconnect and forget remove Monodrive access/configuration only. Automatic sync and cloud-to-cloud server-side transfers are later decisions.

## Verification

Use controlled placeholders or provider mocks for hydration behavior, and a disposable emulator/account only when explicitly configured for integration tests. Cover pagination, interruption, permission errors and unsupported recovery. Never use a live personal bucket as a default fixture.
