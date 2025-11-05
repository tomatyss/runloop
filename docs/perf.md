# Performance Harness

This note captures the cold-start methodology, hardware envelope, and commands that back Epic C’s acceptance criteria. The doc is normative for the current workspace; CI perf jobs reference it when validating thresholds.

## Hardware Profile

All threshold numbers assume the baseline lab node below. When running on different hardware, record the delta alongside the measurements.

- Debian 12 (kernel 6.x)
- x86_64 with 8 logical cores (11th-gen Intel i7 class) or Apple Silicon M-class equivalent
- 16 GB RAM, NVMe SSD
- CPU governor set to `performance`; background services minimal
- Wasmtime 38.0.3 (pinned via `crates/runtime/Cargo.toml`)
- Build profile: `cargo test --workspace` (debug), `cargo bench` (release)

## Cold-start Integration Gate

- Command: `cargo test -p runloop-runtime cold_start_p50_under_40ms`
- Scenario: spawn 50 idle WASM agents, capture wall-clock spawn time, assert median < 40 ms.
- Assertions: non-zero RSS (all platforms) and `cpu_total_ms` reported on Linux builds with default features.
- Output: test failure message includes the observed median when the threshold is exceeded.

## Criterion Benchmark (Informational)

- Command: `cargo bench -p runloop-runtime cold_start`
- Purpose: capture distribution statistics (p50/p90) for cold-start latency; results are uploaded as CI artifacts but not committed.
- Consumption: engineers reviewing perf regressions compare the latest artifact against the historical trend.

## Representative Result (Illustrative)

| Build | Median | P90 | Notes |
|-------|--------|-----|-------|
| debug (`cargo test`) | 31 ms | 36 ms | Debian 12, i7‑1185G7, governor `performance` |
| release (`cargo bench`) | 24 ms | 28 ms | Same host | 

Numbers above are examples taken from the baseline lab node; they are not updated automatically. When recording new measurements, include hardware + commit hash in the accompanying doc or ticket.

## CI Expectations

- The integration test above runs in the `perf-cold-start` CI job on hardware matching the baseline profile. Build failures gate merges.
- Criterion benchmarks run in the same job; results are attached as artifacts for manual inspection.
- Regressions >10% versus the baseline require investigation before landing.

## Updating This Document

- When hardware changes, update the profile table and note the effective date.
- If thresholds move (e.g., Phase 6 budgets tighten), amend both the test assertions and the documentation here.
- Keep examples concise; avoid copying full benchmark output into the repo.
