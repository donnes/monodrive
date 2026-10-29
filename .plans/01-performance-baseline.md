# Performance baseline and remaining budget work

Status: instrumentation and measured baselines exist. Sorting and launch still have documented budget misses. See [performance budgets](../docs/performance.md) for the current numbers and measurement commands; do not maintain a second conflicting budget table here.

## Existing foundation

- `monodrive-core/benches/listing.rs` measures listing and sorting.
- `monodrive/benches/scrolling.rs` measures the real browser with the headless renderer.
- The `profile` feature reports launch and frame timing.

## Remaining work

Investigate the name/kind sorting and launch-to-listing misses recorded in the performance document. Measure before and after on the same machine and review any deliberate budget change. Reuse existing profiling before adding tracing. No runtime test run is implied by this planning review.

All later features preserve responsive selection and scrolling, bounded thumbnail work, and no continuous idle repaints. Network latency, archive work and hashing get separate throughput, cancellation and memory measurements; do not impose a local name-index latency target on them. Do not start the live index for tests or ordinary headless snapshots.
