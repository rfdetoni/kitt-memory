# Changelog

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
