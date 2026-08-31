use kitt_memory_core::*;
use kitt_memory_sqlite::SqliteMemoryStore;
use rusqlite::Connection;
use std::sync::Arc;
use std::thread;

fn temp_db_path(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("kitt-test-{}-{}-{}.db", prefix, std::process::id(), nanos);
    dir.join(name)
}

#[test]
fn test_duplicate_active_memories() {
    let db = temp_db_path("dedup");
    let store = SqliteMemoryStore::open(&db).unwrap();

    let m1 = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::ProjectRule,
            content: "Always run tests before committing".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.9,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();

    // Insert identical memory (same namespace, workspace_id, content)
    let m2 = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::ProjectRule,
            content: "  always RUN tests before COMMITTING   ".into(), // exact normalized match
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.9,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();

    assert_eq!(m1.id, m2.id);

    let recalled = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            text: "tests committing".into(),
            limit: 10,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(recalled.len(), 1);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_pinned_ordering_and_decision_priority() {
    let db = temp_db_path("ordering");
    let store = SqliteMemoryStore::open(&db).unwrap();

    store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::Episodic,
            content: "Temporary note about database migration".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.5,
            confidence: 0.5,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();

    let pinned = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::ArchitectureDecision,
            content: "Architecture rule: database is SQLite WAL".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.9,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();

    let recalled = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            text: "database".into(),
            limit: 2,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();

    assert_eq!(recalled.len(), 2);
    assert_eq!(recalled[0].id, pinned.id);
    assert!(recalled[0].pinned);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_access_count_and_timestamp_touch() {
    let db = temp_db_path("touch");
    let store = SqliteMemoryStore::open(&db).unwrap();

    let mem = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::TechnicalFact,
            content: "Rust version is 1.85+".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    assert_eq!(mem.access_count, 0);
    assert_eq!(mem.last_accessed_at, None);

    // Recall once
    let recalled = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            text: "Rust version".into(),
            limit: 5,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(recalled.len(), 1);

    // Recall second time - access count in database should have incremented
    let recalled2 = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            text: "Rust version".into(),
            limit: 5,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(recalled2.len(), 1);
    assert_eq!(recalled2[0].access_count, 1);
    assert!(recalled2[0].last_accessed_at.is_some());

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_expired_and_superseded_exclusion() {
    let db = temp_db_path("exclusion");
    let store = SqliteMemoryStore::open(&db).unwrap();

    // 1. Expired memory
    let _expired = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::Episodic,
            content: "Short lived ephemeral reminder".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.5,
            confidence: 0.5,
            pinned: false,
            ttl_seconds: Some(0), // expires immediately
            metadata_json: "{}".into(),
        })
        .unwrap();

    // 2. Active memory
    let active = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::ProjectRule,
            content: "Active standard rule".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();

    // 3. Mark active as superseded
    let superseded = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            kind: MemoryKind::ProjectRule,
            content: "Old superseded rule".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.7,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    store
        .set_status(&superseded.id, MemoryStatus::Superseded, Some(&active.id))
        .unwrap();

    // Wait a brief moment to ensure epoch comparison treats ttl_seconds: 0 as expired
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let recalled = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-1".into(),
            text: "rule reminder".into(),
            limit: 10,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();

    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].id, active.id);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_unicode_and_ptbr_content() {
    let db = temp_db_path("unicode");
    let store = SqliteMemoryStore::open(&db).unwrap();

    let content = "Configuração de autenticação: padrão não-bloqueante com símbolos ✨ e acentuação: ação, café, coração.";
    let mem = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-pt".into(),
            kind: MemoryKind::ProjectRule,
            content: content.into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.9,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: r#"{"origem": "especificação_pt_br"}"#.into(),
        })
        .unwrap();

    let recalled = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-pt".into(),
            text: "configuração autenticação coração".into(),
            limit: 5,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();

    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].content, content);
    assert_eq!(recalled[0].id, mem.id);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_concurrent_sqlite_readers_writers() {
    let db = temp_db_path("concurrency");
    let store = Arc::new(SqliteMemoryStore::open(&db).unwrap());

    let mut handles = Vec::new();
    for i in 0..8 {
        let store_clone = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for j in 0..10 {
                let text = format!("Memory item thread {i} iteration {j}");
                store_clone
                    .remember(NewMemory {
                        namespace: "concurrency-ns".into(),
                        workspace_id: "ws-conc".into(),
                        kind: MemoryKind::TechnicalFact,
                        content: text,
                        sensitivity: Sensitivity::Private,
                        scope: MemoryScope::Workspace,
                        importance: 0.5,
                        confidence: 1.0,
                        pinned: false,
                        ttl_seconds: None,
                        metadata_json: "{}".into(),
                    })
                    .unwrap();

                let _ = store_clone
                    .recall(&RecallQuery {
                        namespace: "concurrency-ns".into(),
                        workspace_id: "ws-conc".into(),
                        text: format!("thread {i}"),
                        limit: 5,
                        allow_private: true,
                        allow_secret: true,
                    })
                    .unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let all = store
        .recall(&RecallQuery {
            namespace: "concurrency-ns".into(),
            workspace_id: "ws-conc".into(),
            text: "Memory item".into(),
            limit: 50,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(all.len(), 50);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn test_migration_from_anonymized_agent_cli_db() {
    let source_path = temp_db_path("agent_cli_source");
    let dest_path = temp_db_path("kitt_dest");

    // 1. Create a legacy agent-cli schema DB
    {
        let conn = Connection::open(&source_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE workspaces (
                id TEXT PRIMARY KEY,
                canonical_path_hash TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                git_root TEXT,
                created_at REAL NOT NULL,
                last_opened_at REAL NOT NULL
            );
            CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                normalized_content TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'ACTIVE',
                importance REAL NOT NULL DEFAULT 0.5,
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                last_accessed_at REAL,
                access_count INTEGER NOT NULL DEFAULT 0,
                valid_from REAL,
                valid_until REAL,
                supersedes_id TEXT,
                content_hash TEXT NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                metadata_json TEXT DEFAULT '{}',
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );
            INSERT INTO workspaces VALUES('ws-test', 'hash123', 'Test Workspace', '/tmp/test', 1700000000.0, 1700000000.0);
            INSERT INTO memories VALUES('mem-1', 'ws-test', 'PROJECT_RULE', 'Rule 1: Always test code', 'rule 1: always test code', 'ACTIVE', 0.9, 1.0, 1700000000.0, 1700000000.0, 1700000100.0, 3, NULL, NULL, NULL, 'hash_mem_1', 1, '{"source": "test"}');
            INSERT INTO memories VALUES('mem-2', 'ws-test', 'ARCHITECTURE_DECISION', 'Rule 2: SQLite WAL', 'rule 2: sqlite wal', 'ACTIVE', 0.8, 1.0, 1700000050.0, 1700000050.0, NULL, 0, NULL, NULL, NULL, 'hash_mem_2', 0, '{}');
            INSERT INTO memories VALUES('mem-3', 'ws-test', 'EPISODIC', 'Expired note', 'expired note', 'ACTIVE', 0.5, 0.8, 1700000000.0, 1700000000.0, NULL, 0, NULL, 1700000010.0, NULL, 'hash_mem_3', 0, '{}');
            "#,
        )
        .unwrap();
    }

    // Record source file hash before migration
    let source_bytes_before = std::fs::read(&source_path).unwrap();

    // 2. Perform migration
    let dest_store = SqliteMemoryStore::open(&dest_path).unwrap();
    let count = dest_store.import_legacy_agent_db(&source_path).unwrap();
    assert_eq!(count, 3);

    // 3. Verify destination records
    let recalled = dest_store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws-test".into(),
            text: "Always test code".into(),
            limit: 5,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(recalled.len(), 2);
    assert_eq!(recalled[0].id, "mem-1");
    assert_eq!(recalled[0].content, "Rule 1: Always test code");
    assert!(recalled[0].pinned);
    assert_eq!(recalled[0].access_count, 3);

    // 4. Verify source DB is unchanged
    let source_bytes_after = std::fs::read(&source_path).unwrap();
    assert_eq!(
        source_bytes_before, source_bytes_after,
        "Source database must remain untouched by migration"
    );

    // 5. Test idempotency: re-running migration should succeed without duplicate growth
    let count2 = dest_store.import_legacy_agent_db(&source_path).unwrap();
    assert_eq!(count2, 3);

    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&dest_path);
}
