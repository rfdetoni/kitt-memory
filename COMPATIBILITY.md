# K.I.T.T. Memory — current ecosystem contract

Version 0.5.x intentionally drops historical Agent-memory compatibility.

## Authority

`kitt-memoryd` is the single durable memory authority. The Agent owns conversation/history execution state and Dreaming orchestration, while memory records, corrections, concepts, links, provenance, dream-run persistence and maintenance are stored by `kitt-memory`.

## Current consumers

- `kitt-agent-cli`: authenticated memory service for project memory and reusable knowledge.
- `kitt-assistant`: shared memory domain/storage boundary.
- `kitt-protocol`: shared memory wire envelope.
- `kitt-ai-workers`: outside durable memory authority.

There is no Agent-local fallback database and no legacy database import path in 0.5.x. Obsolete local state is recreated by its owning component instead of reviving a second memory authority.

## 0.8.0 reliability changes

SQLite schema 8 is forward-only: stop an older daemon before upgrading. Protocol
wire version 1 remains compatible. Requests are limited to 1 MiB, 64 concurrent
connections, a two-second absolute frame deadline and eight-second socket writes.

Mutations admit a durable request receipt before changing storage. Identical IDs
replay a completed response; conflicting payloads are rejected. A crash between
mutation and receipt completion leaves `outcome_unknown`, which must be reconciled
before a new write. Query `memory.manage.request` with operation `request.status`
and `arguments.request_id`. Receipt data is bounded to 10,000 rows / 64 MiB;
completed receipts expire after seven days and pending receipts are never evicted.
This is a duplicate-dispatch barrier, not a claim of exactly-once transactions.
