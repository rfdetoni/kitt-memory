# KITT Memory

> Shared persistent memory engine for the KITT ecosystem, built in Rust with SQLite WAL.

Provides high-performance structured memory storage, lexical ranking, exact deduplication, expiry pruning, sensitivity egress enforcement, and migration utilities for Agent CLI databases.

---

## ✨ Features

- **Pure Rust Domain Core**: Zero GUI or HTTP network dependencies (`crates/kitt-memory-core`).
- **SQLite WAL Adapter**: Concurrency-safe SQLite engine with write-locks and busy-timeouts (`crates/kitt-memory-sqlite`).
- **Exact-Content Deduplication**: SHA-256 normalized hash indices prevent active memory row inflation.
- **Lexical & Pinned Retrieval**: Recency, frequency, importance, and pinned priorities built into scoring.
- **Sensitivity & Privacy Egress**: Configurable levels (`public`, `personal`, `private`, `secret`, `ephemeral`) guaranteeing secrets never reach remote providers.
- **Legacy Migration CLI**: Idempotent migration tool to import legacy `kitt-agent-cli` databases without mutating source storage (`apps/kitt-memory-migrate`).

---

## 🏗️ Crates Structure

```
crates/
├── kitt-memory-core/    # Pure domain abstractions, MemoryStore trait, scoring
└── kitt-memory-sqlite/  # SQLite WAL implementation, indexing, queries
apps/
└── kitt-memory-migrate/ # Standalone migration binary
```

---

## 🚀 Quick Start

### Library Usage

```rust
use kitt_memory_core::{MemoryKind, MemoryScope, MemoryStore, NewMemory, RecallQuery, Sensitivity};
use kitt_memory_sqlite::SqliteMemoryStore;

let store = SqliteMemoryStore::open("~/.config/kitt/assistant/memory.db")?;

// Store memory
let mem = store.remember(NewMemory {
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

// Recall memory
let results = store.recall(&RecallQuery {
    namespace: "agent-cli".into(),
    workspace_id: "my-project".into(),
    text: "tests committing".into(),
    limit: 5,
    allow_private: true,
    allow_secret: false,
})?;
```

### Migration Tool

```bash
cargo run -p kitt-memory-migrate -- /path/to/legacy_agent_history.db /path/to/kitt_memory.db
```

---

## 🧪 Testing

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 📄 License

MIT License. See [LICENSE](LICENSE).
