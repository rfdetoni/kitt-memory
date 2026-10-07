# kitt-memory 0.9.3 — Reader pool reuse

Request receipt lookup and concept graph expansion return SQLite readers to the pool on both success and error. Repeated queries no longer drain the pool and reopen connections. Storage schemas and wire framing are unchanged.

## Verification

Regression checks cover the concrete bugs fixed by this release. Native changes are validated with Rust formatting, Clippy, workspace tests and a Python 3.14 wheel integration. Live provider accounts and STT model inference are not part of these local checks.
