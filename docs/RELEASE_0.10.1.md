# Memory 0.10.1 — prevent stale asynchronous embeddings

- Require SHA-256 `expected_content_hash` for `vector.upsert`.
- Atomically reject pending embeddings that reference an older content revision.
- Add regression for stale/current hash; retain scope, sensitivity and search rules.
- Bump Rust workspace + Cargo.lock package versions. No SQLite migration is required.
