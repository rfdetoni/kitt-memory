# K.I.T.T. Memory

<p align="center">
  <strong>Persistent local memory engine for the K.I.T.T. ecosystem.</strong><br>
  Rust · SQLite WAL/FTS5 · hybrid-ready retrieval · deterministic baselines · privacy-aware egress
</p>

<p align="center">
  <a href="https://github.com/rfdetoni/kitt-memory/blob/main/LICENSE"><img alt="License MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-native-000000?logo=rust&logoColor=white">
  <img alt="SQLite WAL" src="https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white">
</p>

K.I.T.T. Memory is the shared persistent memory data plane used across the ecosystem. It stores structured memories locally, retrieves them through bounded FTS5 + retention-aware ranking with an optional semantic reranker, builds deterministic prompt baselines, keeps semantic consolidation conservative, and preserves privacy sensitivity monotonically across updates and migrations.

---

## What’s included

- Pure Rust memory-domain core.
- SQLite WAL storage adapter with busy-timeout and write coordination.
- Exact-content SHA-256 deduplication plus conservative near-duplicate review candidates.
- SQLite FTS5 candidate retrieval with retention-aware ranking.
- Optional semantic reranking through a product-neutral `SemanticReranker` port; lexical/local behavior remains the fallback.
- Deterministic, token-bounded memory baselines with explicit budget pressure and dropped-entry counts.
- Shared correction ledger for learning from prior mistakes.
- Shared concepts and weighted typed knowledge links without coupling the memory crate to Agent session semantics.
- Workspace and namespace scoping.
- Sensitivity levels: `public`, `personal`, `private`, `secret`, `ephemeral`.
- Monotonic sensitivity enforcement on upsert, import and deduplication.
- TTL/expiry pruning.
- Migration utility for legacy K.I.T.T. Agent databases.

---

## Quick links

- **K.I.T.T. ecosystem:** https://github.com/rfdetoni/kitt
- **Agent CLI:** https://github.com/rfdetoni/kitt-agent-cli
- **Assistant:** https://github.com/rfdetoni/kitt-assistant
- **Protocol:** https://github.com/rfdetoni/kitt-protocol

---

## Architecture

```text
crates/
├── kitt-memory-core/     domain types, MemoryStore trait, ranking rules
└── kitt-memory-sqlite/   SQLite WAL implementation, indexes and queries

apps/
└── kitt-memory-migrate/  idempotent legacy migration utility
```

The domain core remains independent of GUI, HTTP and model-provider concerns. Storage-specific behavior is isolated behind store abstractions, while semantic ranking is injected through a small scoring port so the shared engine never requires a resident model service.

---

## Memory model

A memory carries more than text. Retrieval and egress decisions can use its namespace, workspace scope, kind, importance, confidence, pinned status, expiry and sensitivity.

The key privacy invariant is monotonic sensitivity:

```text
result = most_restrictive(existing.sensitivity, incoming.sensitivity)
```

A duplicate merge, migration or later update therefore cannot silently downgrade a memory from `secret` to `private` or from `private` to `public`.

---

## Library usage

```rust
use kitt_memory_core::{
    MemoryKind,
    MemoryScope,
    MemoryStore,
    NewMemory,
    RecallQuery,
    Sensitivity,
};
use kitt_memory_sqlite::SqliteMemoryStore;

let store = SqliteMemoryStore::open("~/.config/kitt/assistant/memory.db")?;

store.remember(NewMemory {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    kind: MemoryKind::ProjectRule,
    content: "Always run tests before committing".into(),
    sensitivity: Sensitivity::Private,
    scope: MemoryScope::Workspace,
    importance: 0.9,
    confidence: 1.0,
    pinned: true,
    ttl_seconds: None,
    metadata_json: "{}".into(),
})?;

let results = store.recall(&RecallQuery {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    text: "tests committing".into(),
    limit: 5,
    allow_private: true,
    allow_secret: false,
})?;

let baseline = store.baseline(&kitt_memory_core::BaselineQuery {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    max_tokens: 500,
    allow_private: true,
    allow_secret: false,
})?;
```

---

## Migration

Import a legacy Agent database without mutating the source:

```bash
cargo run -p kitt-memory-migrate -- \
  /path/to/legacy_agent_history.db \
  ~/.config/kitt/assistant/memory.db
```

Migration is intended to be idempotent and uses the same deduplication and sensitivity invariants as normal writes.

---

## Performance model

The engine is optimized for a local, persistent workload rather than an external vector-database dependency:

- WAL supports concurrent readers while writes remain coordinated.
- Exact hashes prevent duplicate-row inflation; non-exact similarity is surfaced for review instead of being merged blindly.
- FTS5 narrows lexical candidates before scoring, with bounded high-salience fallback candidates to preserve durable rules.
- Retention ranking combines importance, confidence, freshness, access frequency and pinned state.
- Optional semantic scoring reranks only the bounded candidate set; it is not a storage dependency.
- Baseline generation is deterministic and token-bounded, which helps stable prompt prefixes and exposes memory pressure instead of silently hiding it.
- Expired rows can be pruned instead of remaining permanent context baggage.

This keeps memory useful to the Agent without making memory retrieval itself a network dependency.

---

## Security & privacy

Memory sensitivity is part of the data model, not a presentation hint. Callers can explicitly disallow private or secret records from retrieval paths that may leave the machine.

Important guarantees include monotonic sensitivity, local SQLite storage, workspace/namespace scoping and non-destructive legacy migration.

The consuming component is still responsible for applying its own egress and authorization policy before sending recalled content to remote providers.

---

## Testing & linting

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

---

## Contributing

Memory changes should preserve deterministic ranking behavior, migration idempotency and the monotonic sensitivity invariant. Avoid features that require a heavyweight resident service when the same behavior can remain local and bounded.

---

## K.I.T.T. ecosystem

| Repository | Responsibility |
| --- | --- |
| [`kitt`](https://github.com/rfdetoni/kitt) | installer and ecosystem composition |
| [`kitt-agent-cli`](https://github.com/rfdetoni/kitt-agent-cli) | autonomous agent control plane |
| [`kitt-reverse-proxy`](https://github.com/rfdetoni/kitt-reverse-proxy) | authorized provider gateway |
| [`kitt-protocol`](https://github.com/rfdetoni/kitt-protocol) | shared contracts and SDKs |
| [`kitt-toolbox`](https://github.com/rfdetoni/kitt-toolbox) | native code/system data plane |
| [`kitt-ai-workers`](https://github.com/rfdetoni/kitt-ai-workers) | isolated AI/ML workers and evals |
| [`kitt-assistant`](https://github.com/rfdetoni/kitt-assistant) | resident assistant and Control Center |

---

## License

MIT. See [LICENSE](LICENSE).


## Consolidation safety

`find_merge_candidates` returns conservative assessments rather than automatically collapsing paraphrases. Exact normalized content remains the only unconditional merge path. Similar entries, kind changes and polarity/negation changes are surfaced as review candidates so Agent-side Dreaming can decide whether to keep, merge or supersede them using evidence.

## Shared learning primitives

`KnowledgeStore` provides product-neutral corrections, concepts and links. These are intentionally independent from Agent conversation/history tables. The Agent can keep session evidence and Dreaming orchestration in its own repository while gradually moving durable reusable knowledge into the shared memory data plane.
