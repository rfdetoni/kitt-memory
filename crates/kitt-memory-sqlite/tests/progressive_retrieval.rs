use kitt_memory_core::{
    MemoryKind, MemoryScope, MemoryStore, NewMemory, NewMemorySource, SemanticMemoryStore,
    Sensitivity,
};
use kitt_memory_sqlite::SqliteMemoryStore;
use std::fs;
use std::path::{Path, PathBuf};

fn temp_db(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("db-wal"));
    let _ = fs::remove_file(path.with_extension("db-shm"));
}

fn remember(
    store: &SqliteMemoryStore,
    workspace: &str,
    content: &str,
    sensitivity: Sensitivity,
    scope: MemoryScope,
    scope_key: Option<&str>,
) -> String {
    store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: workspace.into(),
            kind: MemoryKind::TechnicalFact,
            content: content.into(),
            sensitivity,
            scope,
            scope_key: scope_key.map(str::to_string),
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap()
        .id
}

#[test]
fn get_many_scoped_preserves_requested_order_and_privacy() {
    let path = temp_db("kitt-memory-progressive-get");
    let store = SqliteMemoryStore::open(&path).unwrap();
    let a = remember(
        &store,
        "ws",
        "first visible fact",
        Sensitivity::Private,
        MemoryScope::Workspace,
        None,
    );
    let b = remember(
        &store,
        "ws",
        "second secret fact",
        Sensitivity::Secret,
        MemoryScope::Workspace,
        None,
    );

    let visible = store
        .get_many_scoped(
            "agent-cli",
            "ws",
            None,
            &[b.clone(), a.clone()],
            true,
            false,
        )
        .unwrap();
    assert_eq!(
        vec![a.clone()],
        visible.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
    );

    let all = store
        .get_many_scoped("agent-cli", "ws", None, &[b.clone(), a.clone()], true, true)
        .unwrap();
    assert_eq!(
        vec![b, a],
        all.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
    );
    cleanup(&path);
}

#[test]
fn timeline_filters_by_source_and_conversation_scope() {
    let path = temp_db("kitt-memory-progressive-timeline");
    let store = SqliteMemoryStore::open(&path).unwrap();
    let conv = remember(
        &store,
        "ws",
        "conversation scoped fact",
        Sensitivity::Private,
        MemoryScope::Conversation,
        Some("conv-1"),
    );
    let other = remember(
        &store,
        "ws",
        "other conversation fact",
        Sensitivity::Private,
        MemoryScope::Conversation,
        Some("conv-2"),
    );

    store
        .record_source(NewMemorySource {
            memory_id: conv.clone(),
            source_kind: "session".into(),
            source_id: "session-1".into(),
            source_uri: Some("kitt://session/session-1".into()),
            source_digest: Some("abc".into()),
            relationship: "evidence".into(),
            source_revision: Some("1".into()),
            observed_at: None,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();
    store
        .record_source(NewMemorySource {
            memory_id: other,
            source_kind: "session".into(),
            source_id: "session-2".into(),
            source_uri: Some("kitt://session/session-2".into()),
            source_digest: Some("def".into()),
            relationship: "evidence".into(),
            source_revision: Some("1".into()),
            observed_at: None,
            valid_from: None,
            valid_until: None,
        })
        .unwrap();

    let rows = store
        .timeline_memories(
            "agent-cli",
            "ws",
            Some("session-1"),
            Some("conv-1"),
            None,
            10,
            true,
            false,
        )
        .unwrap();
    assert_eq!(
        vec![conv],
        rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
    );
    cleanup(&path);
}
