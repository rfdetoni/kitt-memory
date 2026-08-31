# Agent CLI memory compatibility

The shared engine is intentionally evolutionary, not a rewrite of `kitt-agent-cli` memory.

| Agent CLI concept | Shared engine v0.1 |
|---|---|
| `workspace_id` | preserved |
| memory kind | preserved; Assistant adds episodic/personal/routine kinds |
| ACTIVE/SUPERSEDED/ARCHIVED | preserved |
| importance/confidence | preserved |
| created/updated/access timestamps | preserved |
| `valid_until` | preserved as TTL/expiry |
| `supersedes_id` | preserved |
| content hash | preserved/imported |
| pinned | preserved |
| metadata JSON | preserved |
| evidence provenance | remains authoritative in Agent CLI during v0.1 migration |
| Dream runs/plans | remain authoritative in Agent CLI during v0.1 migration |
| namespace | new (`agent-cli`, `assistant`, future products) |
| sensitivity | new (`public`, `personal`, `private`, `secret`, `ephemeral`) |
| scope | new (`global`, `workspace`, `conversation`) |

## Why advanced Dreaming is not rewritten yet

The current Dreaming implementation depends on Agent CLI session/history semantics. Moving it before a stable, product-neutral session-evidence port exists would couple `kitt-memory` back to `kitt-agent-cli` and violate Clean Architecture. v0.1 therefore shares durable/retrieval primitives and leaves advanced consolidation in place. A later extraction must be driven by characterization tests and a generic evidence/session port.
