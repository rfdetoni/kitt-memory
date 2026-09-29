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
