# K.I.T.T. Memory

<p align="center">
  <strong>Persistent local memory engine for the K.I.T.T. ecosystem.</strong><br>
  Rust · SQLite WAL/FTS5 · scope-isolated hybrid retrieval · deterministic baselines · privacy-aware egress
</p>

<p align="center">
  <a href="https://github.com/rfdetoni/kitt-memory/blob/main/LICENSE"><img alt="License MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/Rust-native-000000?logo=rust&logoColor=white">
  <img alt="SQLite WAL" src="https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white">
</p>

K.I.T.T. Memory is the shared persistent memory data plane used across the ecosystem. It stores structured memories locally, retrieves them through bounded FTS5 + retention-aware ranking with an optional semantic reranker, builds deterministic prompt baselines, keeps semantic consolidation conservative, and preserves privacy sensitivity monotonically across updates and migrations.

---

## Memory 0.7.0 — progressive retrieval with memory-owned budgets

Memory 0.7.0 makes progressive retrieval the normal Agent-facing path. `memory.search` returns ranked snippets and provenance under a caller-provided token budget, `memory.get` hydrates only explicitly selected IDs under a second budget, and `memory.timeline` exposes bounded temporal/source-scoped history. The daemon remains responsible for enforcing scope, sensitivity and token pressure; consumers no longer depend on a fixed `limit=8` as the primary context-size control.

The older `memory.recall` endpoint remains a transitional/internal primitive for compatibility during coordinated upgrades, but current ecosystem consumers should use search → get and record consumption receipts against the returned trace IDs.

---

## Memory 0.6.1 — public lifecycle evidence

Memory 0.6.1 exposes a neutral lifecycle ingress for external K.I.T.T. clients: `session.started`, `turn.started`, `tool.completed`, `turn.completed` and `session.ended`. These hooks do not write semantic memories directly. They enqueue the same durable, idempotent `MemoryJob` pipeline used by internal memory work, after validating the event class and reducing source identity/revision to anonymized hashes.

Lifecycle callers provide only a SHA-256 digest of their evidence. Raw tool arguments, prompts, credentials and recalled-memory bodies are not persisted through this interface, and `memory`/`recall` sources are rejected to prevent recalled context from being learned again as fresh evidence.

---

## Memory 0.6.0 — consumption evidence and durable jobs

Memory 0.6.0 makes recall consumption and background memory work auditable without moving orchestration back into the Agent. Runtime recall returns a durable `recall_trace_id`; consumers can record `MemoryConsumptionReceipt` entries that separately represent presentation, reference and action use. This prevents a recall hit from being treated as proof that the model actually used the memory.

Background extraction/consolidation work can be represented by durable `MemoryJob` rows with idempotency keys, worker leases, attempts and retry timestamps. The SQLite schema is v7 and `kitt-memoryd` remains the only durable semantic-memory authority.

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
- Temporal memory validity with `valid_from` / `valid_until`, point-in-time recall and validity-closing supersession.
- Bounded concept-neighborhood expansion (up to four hops) for graph-aware retrieval.
- Hierarchical context nodes with summary/navigation layers, dirty propagation and FTS-backed branch discovery.
- Typed memory provenance, durable consolidation ChangeSets and recall traces.
- Consumption receipts that distinguish presentation, reference and action use.
- Idempotent durable memory jobs with leases, retries and worker-safe claiming.
- Versioned product/plugin memory schemas without expanding the built-in memory-kind enum.
- Namespace, global/workspace and conversation scoping with explicit `scope_key` isolation.
- Sensitivity levels: `public`, `personal`, `private`, `secret`, `ephemeral`.
- Monotonic sensitivity enforcement on upsert and deduplication.
- TTL/expiry pruning.

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
└── kitt-memoryd/         authenticated loopback memory authority
```

The domain core remains independent of GUI, HTTP and model-provider concerns. Storage-specific behavior is isolated behind store abstractions, while semantic ranking is injected through a small scoring port so the shared engine never requires a resident model service.

---

## Memory model

A memory carries more than text. Retrieval and egress decisions can use its namespace, workspace scope, kind, importance, confidence, pinned status, temporal validity and sensitivity. New durable memories start with `valid_from = now`; recall and baselines ignore facts that are not valid yet or are already expired. Superseding/archiving closes an open validity interval instead of silently erasing history.

The key privacy invariant is monotonic sensitivity:

```text
result = most_restrictive(existing.sensitivity, incoming.sensitivity)
```

A duplicate merge or later update therefore cannot silently downgrade a memory from `secret` to `private` or from `private` to `public`.

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
    scope_key: None,
    importance: 0.9,
    confidence: 1.0,
    pinned: true,
    ttl_seconds: None,
    metadata_json: "{}".into(),
})?;

let results = store.recall(&RecallQuery {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    scope_key: None,
    text: "tests committing".into(),
    limit: 5,
    as_of: None,
    allow_private: true,
    allow_secret: false,
})?;

let baseline = store.baseline(&kitt_memory_core::BaselineQuery {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    scope_key: None,
    max_tokens: 500,
    as_of: None,
    allow_private: true,
    allow_secret: false,
})?;
```

---

## Runtime service

`kitt-memoryd` is the only durable memory authority used by the current K.I.T.T. ecosystem. Its normal read plane is progressive (`memory.search` → optional `memory.timeline` → `memory.get`) with memory-owned token budgets; its management plane includes status/pin/archive/touch, Dreaming transactions, corrections, concepts, typed concept links, consumption receipts, durable background jobs and privacy-safe lifecycle evidence ingress. Historical Agent-local memory import is intentionally unsupported.

---

## Performance model

The engine is optimized for a local, persistent workload rather than an external vector-database dependency:

- WAL supports concurrent readers while `IMMEDIATE` write transactions enforce cross-process invariants.
- Exact hashes are deduplicated within namespace + scope + scope key + kind; non-exact similarity is surfaced for review instead of being merged blindly.
- FTS5 narrows lexical candidates before scoring, with bounded high-salience fallback candidates to preserve durable rules.
- Retention ranking combines importance, confidence, freshness, access frequency and pinned state.
- Temporal predicates are applied before ranking, backed by a dedicated temporal lookup index.
- Concept search can expand through a bounded, cycle-safe knowledge neighborhood after FTS seed selection.
- Optional semantic scoring reranks only the bounded candidate set; it is not a storage dependency.
- Baseline generation is deterministic and token-bounded, which helps stable prompt prefixes and exposes memory pressure instead of silently hiding it.
- Access-only updates do not rebuild FTS rows; graph expansion batches each frontier instead of opening per-node connections. Expired rows can be pruned.

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

Memory changes should preserve deterministic ranking behavior, the monotonic sensitivity invariant. Avoid features that require a heavyweight resident service when the same behavior can remain local and bounded.

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

`search_concept_neighborhood` combines FTS concept seeds with bounded graph expansion. The default expansion is cycle-safe, scoped to the same namespace/workspace, capped at four hops and one hundred concepts, and does not introduce a graph-database dependency.


## 0.3 semantic context architecture

Schema v5 adds a semantic navigation layer above the existing memory records. Context nodes are projections and indexes; they do not replace `memories` as the durable shared-memory authority.

A node stores a short summary, navigation summary, generation, content digest and bounded freshness counters. Consumers can search nodes first and only expand the most relevant branches before running the existing FTS/retention/graph/semantic ranking pipeline.

```text
query
  -> context node search
  -> relevant branches
  -> memory FTS + retention
  -> concept neighborhood
  -> optional semantic rerank
  -> token budget
```

### Provenance and audit

`memory_sources` records where a memory came from, including source kind/id, optional `kitt://` URI, digest and revision. `memory_change_sets` and `memory_changes` keep the before/after/evidence trail of consolidation operations. `recall_traces` make selection/fallback/token behavior inspectable without storing model chain-of-thought.

### Freshness

Context nodes accumulate `dirty_children` and expose a deterministic dirty ratio. A worker or consumer decides when to regenerate summaries; the memory hot path only records freshness. This keeps expensive summarization outside normal recall.

### Extensible schemas

The schema registry lets a product or plugin define specialized memory shapes, retention and merge policies while retaining a built-in base kind and default sensitivity. Registration is data-driven and versioned.



## 0.6 standalone memory service

`kitt-memoryd` is now the canonical runtime owner of durable KITT memory. It listens on loopback only (default `127.0.0.1:41829`), stores its token under the KITT memory config directory, and stores SQLite state under the KITT memory data directory.

The hot path exposes protocol-v1 `memory.remember`, `memory.search`, `memory.timeline`, `memory.get` and `memory.forget`; `memory.recall` remains a transitional primitive. Recall responses include a durable trace identity so consumers can record presentation/reference/action receipts without conflating retrieval with actual use. The bounded `memory.manage` control plane owns status changes, pin/archive/touch operations, dream-run history, atomic dream commits, corrections, concepts, typed concept links, consumption receipts, leased background jobs and maintenance. Agent-side code may propose work, but only kitt-memory persists semantic memory state and provenance.

Environment overrides: `KITT_MEMORY_ADDR`, `KITT_MEMORY_CONFIG_DIR`, `KITT_MEMORY_DATA_DIR`, `KITT_MEMORY_TOKEN_PATH`, `KITT_MEMORY_DB`.
