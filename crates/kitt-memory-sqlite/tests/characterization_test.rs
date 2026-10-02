use kitt_memory_core::*;
use kitt_memory_sqlite::{SqliteMemoryStore, TimelineMemoryQuery};
use std::sync::Arc;
use std::thread;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
            scope_key: None,
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
            scope_key: None,
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
            scope_key: None,
            text: "tests committing".into(),
            limit: 10,
            as_of: None,
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
            scope_key: None,
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
            scope_key: None,
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
            scope_key: None,
            text: "database".into(),
            limit: 2,
            as_of: None,
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
fn test_search_does_not_touch_but_explicit_hydration_does() {
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
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    assert_eq!(mem.access_count, 0);
    assert_eq!(mem.last_accessed_at, None);

    let query = RecallQuery {
        namespace: "agent-cli".into(),
        workspace_id: "ws-1".into(),
        scope_key: None,
        text: "Rust version".into(),
        limit: 5,
        as_of: None,
        allow_private: true,
        allow_secret: true,
    };
    assert_eq!(store.recall(&query).unwrap()[0].access_count, 0);
    assert_eq!(store.recall(&query).unwrap()[0].access_count, 0);

    store.touch_records(std::slice::from_ref(&mem.id)).unwrap();
    let hydrated = store.recall(&query).unwrap();
    assert_eq!(hydrated[0].access_count, 1);
    assert!(hydrated[0].last_accessed_at.is_some());

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
            scope_key: None,
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
            scope_key: None,
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
            scope_key: None,
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
            scope_key: None,
            text: "rule reminder".into(),
            limit: 10,
            as_of: None,
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
            scope_key: None,
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
            scope_key: None,
            text: "configuração autenticação coração".into(),
            limit: 5,
            as_of: None,
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
                        scope_key: None,
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
                        scope_key: None,
                        text: format!("thread {i}"),
                        limit: 5,
                        as_of: None,
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
            scope_key: None,
            text: "Memory item".into(),
            limit: 50,
            as_of: None,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(all.len(), 50);

    let _ = std::fs::remove_file(&db);
}

#[cfg(unix)]
#[test]
fn test_database_file_is_private_and_symlink_target_is_rejected() {
    let db = temp_db_path("private-mode");
    let store = SqliteMemoryStore::open(&db).unwrap();
    let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    drop(store);
    let _ = std::fs::remove_file(&db);

    let target = temp_db_path("symlink-target");
    std::fs::write(&target, b"not a sqlite database").unwrap();
    let link = temp_db_path("symlink-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(SqliteMemoryStore::open(&link).is_err());
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_file(&target);
}

#[test]
fn conversation_scope_is_isolated() {
    let db = temp_db_path("conversation-scope");
    let store = SqliteMemoryStore::open(&db).unwrap();
    store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::Episodic,
            content: "conversation alpha only".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Conversation,
            scope_key: Some("alpha".into()),
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    let beta = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            scope_key: Some("beta".into()),
            text: "conversation alpha".into(),
            limit: 10,
            as_of: None,
            allow_private: true,
            allow_secret: false,
        })
        .unwrap();
    assert!(beta.is_empty());
    let alpha = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            scope_key: Some("alpha".into()),
            text: "conversation alpha".into(),
            limit: 10,
            as_of: None,
            allow_private: true,
            allow_secret: false,
        })
        .unwrap();
    assert_eq!(alpha.len(), 1);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn independent_stores_never_downgrade_sensitivity() {
    let db = temp_db_path("multi-store-sensitivity");
    let a = SqliteMemoryStore::open(&db).unwrap();
    let b = SqliteMemoryStore::open(&db).unwrap();
    let base = a
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::ProjectRule,
            content: "shared invariant".into(),
            sensitivity: Sensitivity::Public,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    let first = std::thread::spawn(move || {
        a.remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::ProjectRule,
            content: "shared invariant".into(),
            sensitivity: Sensitivity::Secret,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap()
    });
    let second = std::thread::spawn(move || {
        b.remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::ProjectRule,
            content: "shared invariant".into(),
            sensitivity: Sensitivity::Personal,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap()
    });
    first.join().unwrap();
    second.join().unwrap();
    let verify = SqliteMemoryStore::open(&db).unwrap();
    let rows = verify
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            scope_key: None,
            text: "shared invariant".into(),
            limit: 5,
            as_of: None,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, base.id);
    assert_eq!(rows[0].sensitivity, Sensitivity::Secret);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn historical_recall_is_side_effect_free_and_zero_limit_is_empty() {
    let db = temp_db_path("historical");
    let store = SqliteMemoryStore::open(&db).unwrap();
    let memory = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::TechnicalFact,
            content: "historical fact".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    let historical = RecallQuery {
        namespace: "agent-cli".into(),
        workspace_id: "ws".into(),
        scope_key: None,
        text: "historical fact".into(),
        limit: 5,
        as_of: Some(memory.created_at),
        allow_private: true,
        allow_secret: false,
    };
    assert_eq!(store.recall(&historical).unwrap()[0].access_count, 0);
    assert_eq!(store.recall(&historical).unwrap()[0].access_count, 0);
    let mut zero = historical;
    zero.limit = 0;
    assert!(store.recall(&zero).unwrap().is_empty());
    let _ = std::fs::remove_file(&db);
}

#[test]
fn distinct_kinds_and_scopes_do_not_exact_merge() {
    let db = temp_db_path("identity");
    let store = SqliteMemoryStore::open(&db).unwrap();
    let a = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::ProjectRule,
            content: "same text".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    let b = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::TechnicalFact,
            content: "same text".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    assert_ne!(a.id, b.id);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn progressive_hydration_and_timeline_preserve_scope_and_provenance() {
    let db = temp_db_path("progressive");
    let store = SqliteMemoryStore::open(&db).unwrap();
    let record = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws-progressive".into(),
            kind: MemoryKind::ArchitectureDecision,
            content: "The durable event ledger is the execution source of truth.".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Conversation,
            scope_key: Some("conv-a".into()),
            importance: 0.95,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    store
        .record_source(NewMemorySource {
            memory_id: record.id.clone(),
            source_kind: "repository".into(),
            source_id: "session-42".into(),
            source_uri: Some("kitt://repo/docs/architecture".into()),
            source_digest: Some("abc123".into()),
            relationship: "supports".into(),
            source_revision: Some("r1".into()),
            observed_at: None,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let wrong_scope = store
        .get_many_scoped(
            "agent-cli",
            "ws-progressive",
            Some("conv-b"),
            std::slice::from_ref(&record.id),
            true,
            false,
        )
        .unwrap();
    assert!(wrong_scope.is_empty());

    let hydrated = store
        .get_many_scoped(
            "agent-cli",
            "ws-progressive",
            Some("conv-a"),
            std::slice::from_ref(&record.id),
            true,
            false,
        )
        .unwrap();
    assert_eq!(hydrated.len(), 1);
    assert_eq!(hydrated[0].id, record.id);

    let timeline = store
        .timeline_memories(&TimelineMemoryQuery {
            namespace: "agent-cli",
            workspace_id: "ws-progressive",
            source_id: Some("session-42"),
            scope_key: Some("conv-a"),
            around: None,
            limit: 10,
            allow_private: true,
            allow_secret: false,
        })
        .unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].id, record.id);
    let provenance = store.sources_for_memory(&record.id).unwrap();
    assert_eq!(provenance.len(), 1);
    assert_eq!(
        provenance[0].source_uri.as_deref(),
        Some("kitt://repo/docs/architecture")
    );

    let _ = std::fs::remove_file(&db);
}


#[test]
fn migration_v8_to_v9_rehashes_without_data_loss_or_sensitivity_downgrade() {
    use rusqlite::Connection;

    let db = temp_db_path("migration-v9");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_info(version INTEGER NOT NULL);
             INSERT INTO schema_info(version) VALUES(8);
             CREATE TABLE memories(
               id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,
               kind TEXT NOT NULL,content TEXT NOT NULL,normalized_content TEXT NOT NULL,
               status TEXT NOT NULL,sensitivity TEXT NOT NULL,scope TEXT NOT NULL,scope_key TEXT,
               importance REAL NOT NULL,confidence REAL NOT NULL,created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,last_accessed_at INTEGER,access_count INTEGER NOT NULL DEFAULT 0,
               valid_from INTEGER,valid_until INTEGER,supersedes_id TEXT,content_hash TEXT NOT NULL,
               pinned INTEGER NOT NULL DEFAULT 0,metadata_json TEXT NOT NULL DEFAULT '{}'
             );
             INSERT INTO memories VALUES(
               'a','agent-cli','ws','PROJECT_RULE','Ação padrão','ação padrão','ACTIVE','public',
               'workspace',NULL,0.8,1.0,1,1,NULL,0,1,NULL,NULL,'old-a',0,'{}'
             );
             INSERT INTO memories VALUES(
               'b','agent-cli','ws','PROJECT_RULE','acao padrao','acao padrao','ACTIVE','secret',
               'workspace',NULL,0.9,1.0,2,2,NULL,0,2,NULL,NULL,'old-b',1,'{}'
             );",
        )
        .unwrap();
    }

    let store = SqliteMemoryStore::open(&db).unwrap();
    let rows = store
        .recall(&RecallQuery {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            scope_key: None,
            text: "acao padrao".into(),
            limit: 10,
            as_of: None,
            allow_private: true,
            allow_secret: true,
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sensitivity, Sensitivity::Secret);
    assert_eq!(rows[0].normalized_content, "acao padrao");
    assert!(!rows[0].gist.is_empty());
    assert!(rows[0].tokens_est > 0);

    let conn = Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
        .unwrap();
    let superseded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE status='SUPERSEDED'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, 9);
    assert_eq!(total, 2);
    assert_eq!(superseded, 1);

    drop(conn);
    drop(store);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn baseline_revision_ignores_access_telemetry_but_tracks_semantic_changes() {
    let db = temp_db_path("baseline-revision");
    let store = SqliteMemoryStore::open(&db).unwrap();
    let record = store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            kind: MemoryKind::ProjectRule,
            content: "Run validation before release".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.9,
            confidence: 1.0,
            pinned: true,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap();
    let query = BaselineQuery {
        namespace: "agent-cli".into(),
        workspace_id: "ws".into(),
        scope_key: None,
        max_tokens: 256,
        as_of: None,
        allow_private: true,
        allow_secret: false,
    };

    let first = store.baseline(&query).unwrap();
    store.touch_records(std::slice::from_ref(&record.id)).unwrap();
    let second = store.baseline(&query).unwrap();
    assert_eq!(first.baseline_revision, second.baseline_revision);
    assert_eq!(first.etag, second.etag);

    store.set_pinned(&record.id, false).unwrap();
    let third = store.baseline(&query).unwrap();
    assert!(third.baseline_revision > second.baseline_revision);
    assert_ne!(third.etag, second.etag);

    drop(store);
    let _ = std::fs::remove_file(&db);
}
