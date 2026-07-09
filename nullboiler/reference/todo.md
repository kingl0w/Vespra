# NullBoiler Production Gaps vs OpenClaw-Ready Orchestrators

This backlog tracks the main gaps that prevent `nullboiler` from matching production-ready orchestrators commonly used with OpenClaw-compatible ecosystems.

## P0 (Critical)

- [x] `P0-00` **Data-integrity: single SQLite connection shared across the HTTP and engine threads.** Fixed: `main.zig` now opens a dedicated `engine_store` connection for the engine thread instead of sharing the HTTP thread's handle, so their transactions can no longer interleave on one `sqlite3*`. WAL supports concurrent readers + a single writer across connections; migrations are idempotent. Verified with `zig build test` and the full `tests/test_e2e.sh` (50/50, incl. concurrent multi-step workflows).
- [x] `P0-05` **Named-DAG registry / payload contract.** Resolved on the gateway side: the gateway owns a DAG registry (`gateway-rs/src/dag_registry.rs`) that expands a named workflow into a real multi-step `steps[]` payload, and registers its nine agents as webhook workers with nullboiler at boot (tags = agent name), so nullboiler dispatches each step back to the gateway's `/nullboiler/worker/:agent` endpoints. Verified end-to-end against this binary + the mock worker: a `yield_rotation` workflow runs all steps in dependency order with `{{steps.ID.output}}` / `{{input.X}}` templating resolved.
- [x] `P0-01` API authentication and authorization boundary (token-based access for non-health endpoints).
- [x] `P0-02` Atomic run creation with DB transaction (no partial run/step persistence on failure).
- [ ] `P0-03` Worker health checks + quarantine/circuit-breaker states (dead/draining lifecycle automation).
- [ ] `P0-04` Idempotent run submission (idempotency key to prevent duplicate workflow launches).

## P1 (High)

- [ ] `P1-01` Retry policy upgrades: exponential backoff + jitter + max elapsed time.
- [ ] `P1-02` Dead-letter handling for terminally failed runs/steps.
- [ ] `P1-03` Structured observability: request IDs, metrics endpoint, OTEL spans.
- [ ] `P1-04` API pagination/filtering for runs, steps, events, workers.
- [ ] `P1-05` Admission control and rate limiting (per IP/token and global caps).
- [ ] `P1-06` Graceful shutdown and drain mode for in-flight steps.

## P2 (Medium)

- [ ] `P2-01` Signed callbacks/webhooks (HMAC) and replay protection.
- [ ] `P2-02` Multi-tenant namespace boundaries (projects/tenants/quotas).
- [ ] `P2-03` Workflow versioning and deterministic replay diagnostics.
- [ ] `P2-04` OpenAPI spec + generated client SDK for automation integrations.

## Current Execution

- [x] `P0-01` completed: bearer auth implemented in API path, config/CLI wiring added, and nulltracker bridge updated with optional token propagation.
- [x] `P0-02` completed: `POST /runs` now wrapped in DB transaction with rollback on any failure.
- [ ] Next: `P0-03` worker health checks + circuit-breaker states.
