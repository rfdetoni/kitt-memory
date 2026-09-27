# Security

Report vulnerabilities privately to the repository owner. Do not open public issues containing secrets or exploitable details.

Baseline rules:
- never commit API keys/tokens;
- bind local control surfaces to loopback unless a separately reviewed remote-auth design exists;
- validate untrusted model/tool/memory data at trust boundaries;
- use least privilege and explicit capability checks;
- keep secrets out of logs, telemetry and UI events;
- treat dependency updates as security-sensitive changes and run the full test suite.


Memory-specific invariants:
- conversation-scoped records require an explicit scope key;
- sensitivity may only become more restrictive during merges/upserts;
- corrections and concepts retain sensitivity/provenance from their source memories;
- SQLite uses secure-delete mode and rejects symlink database targets.
