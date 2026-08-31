# KITT Memory Engine

Shared, low-footprint memory engine for KITT products. It preserves the core concepts already used by `kitt-agent-cli` (workspace, kind, status, importance, confidence, evidence/supersession) and adds explicit namespace, scope, sensitivity, TTL and provider-egress policy.

The engine deliberately does **not** replace the Agent CLI's advanced Dreaming Mode in v0.1. It provides deterministic, safe consolidation primitives (expiry and exact-duplicate supersession), while the existing Dreaming implementation remains valid during migration.

## Crates

- `kitt-memory-core`: domain types, port, scoring/egress rules, deterministic consolidation.
- `kitt-memory-sqlite`: SQLite/WAL adapter with indexed retrieval and legacy Agent CLI importer.
- `kitt-memory-migrate`: one-shot migration CLI.

## Build

```bash
cargo test --workspace
cargo run -p kitt-memory-migrate -- /path/to/agent-history.db /path/to/kitt-memory.db
```
