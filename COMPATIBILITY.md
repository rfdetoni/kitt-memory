# Agent CLI memory compatibility

The shared engine is intentionally evolutionary, not a rewrite of `kitt-agent-cli` memory.

| Agent CLI concept | Shared engine v0.1 |
|---|---|
| `workspace_id` | preserved |
| memory kind | preserved; Assistant adds episodic/personal/routine kinds |
| ACTIVE/SUPERSEDED/ARCHIVED | preserved |
| importance/confidence | preserved |
| created/updated/access timestamps | preserved |
| `valid_from` | preserved/imported and enforced during recall/baseline |
| `valid_until` | preserved as expiry and closed on supersession/archive |
| `supersedes_id` | preserved |
| content hash | preserved/imported |
| pinned | preserved |
| metadata JSON | preserved |
| evidence provenance | remains authoritative in Agent CLI during v0.1 migration |
| Dream runs/plans | remain authoritative in Agent CLI; shared memory only exposes product-neutral consolidation candidates |
| deterministic prompt baseline | shared engine now exposes token-bounded baseline snapshots |
| hybrid retrieval | FTS5 + retention ranking in shared engine, optional semantic reranker port |
| corrections | shared product-neutral correction ledger available; Agent-native compatibility remains during migration |
| concepts/links | shared product-neutral knowledge contracts plus bounded neighborhood expansion; session evidence remains Agent-owned |
| namespace | new (`agent-cli`, `assistant`, future products) |
| sensitivity | new (`public`, `personal`, `private`, `secret`, `ephemeral`) |
| scope | new (`global`, `workspace`, `conversation`) |

## Why advanced Dreaming is not rewritten yet

The current Dreaming implementation depends on Agent CLI session/history semantics. Moving it before a stable, product-neutral session-evidence port exists would couple `kitt-memory` back to `kitt-agent-cli` and violate Clean Architecture. v0.1 therefore shares durable/retrieval primitives and leaves advanced consolidation in place. A later extraction must be driven by characterization tests and a generic evidence/session port.


## v0.1.6 temporal/graph convergence

The shared store now preserves the Agent's `valid_from` semantics, enforces temporal validity before retrieval/baseline ranking, closes validity intervals when memories are superseded or archived, and exposes bounded concept-neighborhood expansion for graph-aware consumers. The SQLite schema is v3 and migrates v2 stores in place.

## v0.1.5 convergence

The shared engine now contains the reusable pieces that previously only existed in the Agent's richer local memory layer: bounded hybrid-ready recall, durable baseline construction, correction records and knowledge concepts/links. This does **not** move Agent session evidence, Dream scheduling, provider routing or model orchestration into `kitt-memory`.

The migration direction is therefore one-way: reusable durable knowledge moves toward `kitt-memory`; Agent-specific orchestration stays in `kitt-agent-cli`.
