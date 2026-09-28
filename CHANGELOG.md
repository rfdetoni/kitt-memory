# Changelog

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
