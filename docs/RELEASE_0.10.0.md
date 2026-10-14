# Memory 0.10.0 — opt-in hybrid vector index

## Storage
- SQLite remains the durable authority, v9→v10 forward-only migration.
- `memory_embeddings`: primary key (memory ID, model), normalized f32 BLOB, content hash, dimension, fixed 8-bit LSH bucket, updated time.
- Index `idx_memory_embeddings_probe(model,dimensions,bucket,memory_id)`; delete/update triggers invalidate vectors.
- No new crates: the workspace's three package versions are bumped in Cargo.lock.

## Retrieval
- Authenticated `memory.manage.request` operation `vector.upsert` accepts memory_id, model and embedding array.
- `memory.search.request` can supply `embedding_model` and `query_embedding`; lexical-only stays default.
- Up to 4096 candidates sampled from the exact LSH bucket and eight Hamming-distance-one buckets; authorized status, scope, temporal, content-hash and sensitivity filters execute before cosine scoring.
- Reject cosine under 0.30, combine with FTS5 using reciprocal-rank fusion, then apply result count/token budget.
- `memory.manage` operation `vector.clear` removes a model's derived embeddings.
- **Approximate search, not LanceDB/HNSW.** Embeddings must be produced externally; an automatic embedder is not part of this change. Evaluate Recall@K against exhaustive exact search, model generation latency, 100k+ rows and bucket skew before replacing the backend.

## Verify
```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```
