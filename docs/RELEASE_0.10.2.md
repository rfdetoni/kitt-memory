# Memory 0.10.2 — restart-safe vector backfill

- Expose authenticated, bounded `vector.pending` for workspace-owned records missing the current model/content-hash embedding.
- Derive retry work from SQLite instead of introducing another queue or worker table.
- Scope to the requested namespace/workspace; exclude conversations and secret/ephemeral content from automatic backfill.
- Re-check current memory content hash atomically when writing each embedding.
- Add tests for model isolation, workspace isolation and persistence of missing work.
