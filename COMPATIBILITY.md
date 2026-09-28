# Agent CLI memory compatibility

The shared engine is intentionally evolutionary, not a rewrite of `kitt-agent-cli` memory.

| Agent CLI concept | Shared engine 0.2.x |
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
| evidence provenance | remains authoritative in Agent CLI during 0.2.x migration |
| Dream runs/plans | remain authoritative in Agent CLI; shared memory only exposes product-neutral consolidation candidates |
| deterministic prompt baseline | shared engine now exposes token-bounded baseline snapshots |
| hybrid retrieval | FTS5 + retention ranking in shared engine, optional semantic reranker port |
| corrections | shared product-neutral correction ledger available; Agent-native compatibility remains during migration |
| concepts/links | shared product-neutral knowledge contracts plus bounded neighborhood expansion; session evidence remains Agent-owned |
| namespace | new (`agent-cli`, `assistant`, future products) |
| sensitivity | new (`public`, `personal`, `private`, `secret`, `ephemeral`) |
| scope | `global`, `workspace`, `conversation`; conversation requires `scope_key` from v0.2 |

## Authority and interoperability in 0.2.x

`kitt-memory` is the reusable structured memory data plane. Product integrations must not treat daemon availability as a switch between unrelated authorities. Agent-originated durable records remain visible from the Agent's structured store and may be mirrored into the shared store; shared recall is an additional source that is merged by the Agent. Human-readable Markdown is a recovery/projection format, not a competing structured authority.

Clearing Agent project memory archives the Agent-owned structured records and best-effort deletes exact workspace-scoped shared mirrors. This keeps temporary daemon outages or restarts from resurrecting memories that the Agent has already cleared locally.

## Why advanced Dreaming is not rewritten yet

The current Dreaming implementation depends on Agent CLI session/history semantics. Moving it before a stable, product-neutral session-evidence port exists would couple `kitt-memory` back to `kitt-agent-cli` and violate Clean Architecture. v0.1 therefore shares durable/retrieval primitives and leaves advanced consolidation in place. A later extraction must be driven by characterization tests and a generic evidence/session port.


## v0.1.6 temporal/graph convergence

The shared store now preserves the Agent's `valid_from` semantics, enforces temporal validity before retrieval/baseline ranking, closes validity intervals when memories are superseded or archived, and exposes bounded concept-neighborhood expansion for graph-aware consumers. The SQLite schema is v3 and migrates v2 stores in place.

## v0.1.5 convergence

The shared engine now contains the reusable pieces that previously only existed in the Agent's richer local memory layer: bounded hybrid-ready recall, durable baseline construction, correction records and knowledge concepts/links. This does **not** move Agent session evidence, Dream scheduling, provider routing or model orchestration into `kitt-memory`.

The migration direction is therefore one-way: reusable durable knowledge moves toward `kitt-memory`; Agent-specific orchestration stays in `kitt-agent-cli`.


## v0.2.0 scope/privacy hardening

Schema v4 adds explicit conversation `scope_key`, point-in-time `as_of` retrieval, atomic sensitivity preservation across independent writers, scope/kind-aware exact deduplication, pre-limit sensitivity filtering and provenance/sensitivity for corrections and concepts. Existing schema v3 databases migrate in place. Legacy conversation rows receive the isolated key `legacy`.


## 0.3 semantic hierarchy and provenance

Version 0.3 adds schema-v5 context nodes, provenance, ChangeSets, recall traces and schema registrations as additive shared-data-plane capabilities. Existing Agent-owned durable records remain authoritative for Agent-originated project state. Context nodes are navigation projections, not a new competing authority, and shared-memory availability still must not determine whether Agent-local memories are visible.

Consumers that understand 0.3 can progressively retrieve context-node summaries before normal recall, attach provenance to mirrored memories and persist consolidation audit records. Older consumers can continue using the v0.2 MemoryStore APIs against the same database.
