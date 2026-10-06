# kitt-memory 0.9.2

Add bounded `receipt.record_batch` management (1–128 receipts), validated before a single SQLite transaction. Preserve monotonic/idempotent receipt updates and test all-or-nothing validation. SQLite schema remains v9.

Validation uses Python 3.14, Node 24 and Rust checks where applicable. Cross-repository CI covers supported deployment environments.
