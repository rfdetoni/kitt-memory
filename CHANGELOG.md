# Changelog

## 0.9.2 — 2026-10-06

- Add bounded `receipt.record_batch` management (1–128 receipts), validated before a single SQLite transaction. Preserve monotonic/idempotent receipt updates and test all-or-nothing validation. SQLite schema remains v9.

## 0.9.1 — 2026-10-02

- Isolate `kitt-memoryd` management dispatch into its own module without changing the Protocol 0.9/wire-v1 surface or semantic-memory authority.
- Publish the post-0.9.0 runtime refactor under a new immutable patch version; dependencies and SQLite schema remain unchanged.

## 0.9.0 — 2026-10-02

- Advance SQLite to schema v9 with normalized-content FTS5, derived `gist` / `tokens_est` fields and collision-safe rehash/dedup migration.
- Make search/recall side-effect free; access counters now move only on explicit hydration or positive consumption evidence.
- Add deterministic baseline revisions and ETags plus `memory.baseline.request` / `if_none_match` support.
- Reuse bounded SQLite reader connections, buffer recall traces off the hot path and add periodic trace/job cleanup, FTS optimization and passive WAL checkpoints.
- Gate bounded semantic reranking by lexical ambiguity and restrict salience fallback to sparse/empty lexical result sets.
- Add optional progressive controls for provenance omission and excluded IDs while preserving all protocol-v1 response fields.
- Replace thread-per-connection serving with a fixed worker pool, bounded queue, keep-alive loops and constant-time auth-token comparison.
- Add v8→v9 migration, side-effect-free search, baseline revision and concurrency characterization coverage plus a dependency-free latency benchmark.
- Keep MSRV at Rust 1.88 and enable release LTO/single codegen unit/symbol stripping.

## 0.8.1 — 2026-10-02

- Keep bounded connection admission compatible with Rust 1.88 and current stable Clippy using compare_exchange rather than deprecated fetch_update.

## 0.8.0 — 2026-10-02

- Bound daemon frames, absolute read deadlines, writes and concurrent connections.
- Migrate to SQLite schema 8 with durable mutation request receipts, conflict detection and explicit unknown outcomes.
- Expose request.status for reconciliation; completed receipts expire after seven days, uncertain receipts remain protected.


## 0.7.0 - 2026-10-01

- Promote progressive memory retrieval to the primary runtime contract through `memory.search`, `memory.timeline` and `memory.get`.
- Enforce explicit token budgets inside `kitt-memoryd`: search returns bounded snippets/provenance first and get hydrates only selected memory IDs.
- Preserve namespace/workspace/conversation scope, point-in-time search, sensitivity filters and provenance across progressive retrieval.
- Keep legacy `memory.recall` available only as an internal/transitional primitive while current K.I.T.T. consumers migrate to Protocol 0.6.
- Add regression coverage for scoped hydration ordering, secret filtering and provenance-source timeline queries.


## 0.6.1 - 2026-09-30

- Add the public lifecycle evidence ingress for `session.started`, `turn.started`, `tool.completed`, `turn.completed` and `session.ended`.
- Route lifecycle observations through the existing idempotent `MemoryJob` pipeline instead of creating a second store or bypassing evidence tiering.
- Reject memory/recall-derived lifecycle sources and persist only anonymized source identity/revision plus a caller-provided SHA-256 evidence digest.
- Preserve `kitt-memoryd` as the single durable semantic-memory authority.

## 0.6.0 - 2026-09-30

- Return a durable `recall_trace_id` with runtime recall so consuming agents can correlate exactly which memories were selected.
- Add `MemoryConsumptionReceipt` persistence that distinguishes memory merely presented to a model from memory referenced or used for an action.
- Add durable `MemoryJob` orchestration with idempotent enqueue, worker leases, attempts, retry scheduling and terminal completion/failure states.
- Extend `kitt-memoryd` management operations for receipt recording/querying and memory-job enqueue/claim/complete/fail.
- Migrate the SQLite store to schema v7 with receipt/job indexes while preserving Memory as the single durable semantic-memory authority.
- Integrate the Agent CLI 0.80 recall path without reintroducing Agent-local semantic memory ownership.

## 0.5.0 - 2026-09-28

- Remove the legacy Agent-memory import binary and SQLite import path.
- Extend `kitt-memoryd` management with durable correction, concept and typed concept-link operations.
- Make the single-memory-authority contract explicit: reusable knowledge is written only to kitt-memory.
- Drop historical compatibility documentation in favor of the current ecosystem contract.

## 0.4.0 - 2026-09-28

- Make kitt-memory the standalone durable authority for Agent memory.
- Add `kitt-memoryd`, a loopback-only authenticated service for remember/recall/forget and bounded management operations.
- Add schema v6 dream-run persistence and atomic dream commit support.
- Add CANDIDATE memory status used by Dreaming Mode.
- Add bounded administrative reads, status/pin/archive/touch operations and maintenance.
- Keep provenance in `memory_sources` and consolidate/prune in the memory service instead of Agent-local tables.


## 0.3.0 - 2026-09-28

- Add hierarchical context nodes with bounded freshness counters and FTS-backed branch discovery.
- Add explicit memory-to-context links without changing the authority of existing MemoryRecord rows.
- Add typed provenance records linking memories to sessions, task episodes, tool results, repository revisions, tests and other resources.
- Add durable MemoryChangeSet/MemoryChange audit history for consolidation and supersession decisions.
- Add durable recall traces for explainability, token accounting, fallback tracking and context hashes.
- Add a versioned MemorySchema registry so plugins/products can define specialized memory payloads without growing the core enum.
- Migrate SQLite schema to v5 and backfill legacy memories into per-workspace root context nodes.
- Preserve local SQLite/FTS5 authority, monotonic sensitivity and the Agent/shared-memory authority boundary.


## 0.2.1 - 2026-09-27

- Clarify the 0.2.x ownership contract: shared memory is an interoperability data plane, not a daemon-availability-selected replacement for product-owned durable state.
- Document Agent CLI 0.74.4 mirroring/merged-recall behavior and Markdown recovery-only semantics.
- Align compatibility documentation terminology with schema-v4 / 0.2.x behavior.


## 0.2.0 - 2026-09-26

- Add schema v4 with conversation `scope_key` isolation and point-in-time `as_of` recall.
- Make monotonic sensitivity and duplicate writes safe across independent SQLite writers.
- Make exact identity scope/kind-aware, canonicalize global writes and reject persisted enum corruption.
- Filter privacy before candidate limits, preserve high-salience candidates and keep historical recall side-effect free.
- Stop FTS write amplification on access telemetry and batch graph-neighborhood traversal.
- Carry sensitivity/provenance into corrections and concepts and reject malformed JSON/counters.
- Make migrations transactional with integrity checks and safe legacy conversation migration.
- Upgrade rusqlite to 0.40.2, set MSRV to Rust 1.88 and extend CI/security coverage.


## 0.1.6 - 2026-09-24

- Add explicit `valid_from` temporal validity to shared memory records and schema v3.
- Enforce temporal windows in FTS/fallback recall and deterministic baselines.
- Close open validity intervals when memories are superseded, archived or deduplicated.
- Preserve/import legacy Agent `valid_from` when present and derive it from `created_at` otherwise.
- Add bounded cycle-safe concept-neighborhood expansion for graph-aware retrieval.
- Add temporal/graph characterization tests and a temporal lookup index.

## 0.1.5 - 2026-09-24

- Add deterministic token-bounded memory baselines with explicit pressure/drop metadata.
- Add SQLite FTS5 candidate retrieval and retention-aware ranking with optional semantic reranking.
- Add conservative near-duplicate assessments without unsafe automatic semantic merging.
- Add product-neutral correction, concept and knowledge-link persistence.
- Migrate the shared SQLite schema to v2 with idempotent FTS backfill/triggers.
- Keep local/offline lexical retrieval, exact deduplication and monotonic sensitivity guarantees.

## 0.1.0 - Unreleased

- Initial KITT ecosystem foundation.
