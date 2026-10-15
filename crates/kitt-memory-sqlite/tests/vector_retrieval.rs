use kitt_memory_core::{
    MemoryKind, MemoryScope, MemoryStore, NewMemory, RecallQuery, Sensitivity, hash_normalized,
    normalize,
};

fn hash(content: &str) -> String {
    hash_normalized(&normalize(content))
}
use kitt_memory_sqlite::SqliteMemoryStore;

fn store_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "kitt-hybrid-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}
fn put(
    store: &SqliteMemoryStore,
    workspace: &str,
    sensitivity: Sensitivity,
    content: &str,
) -> String {
    store
        .remember(NewMemory {
            namespace: "agent-cli".into(),
            workspace_id: workspace.into(),
            kind: MemoryKind::TechnicalFact,
            content: content.into(),
            sensitivity,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })
        .unwrap()
        .id
}
fn query(workspace: &str) -> RecallQuery {
    RecallQuery {
        namespace: "agent-cli".into(),
        workspace_id: workspace.into(),
        scope_key: None,
        text: "unrelated terminology".into(),
        limit: 5,
        as_of: None,
        allow_private: false,
        allow_secret: false,
    }
}
#[test]
fn hybrid_recovers_vector_only_match_with_scope_and_privacy_filters() {
    let db = store_path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    let visible = put(
        &store,
        "ws1",
        Sensitivity::Public,
        "pluggable retry plan scheduler",
    );
    let other_ws = put(&store, "ws2", Sensitivity::Public, "another project");
    let secret = put(
        &store,
        "ws1",
        Sensitivity::Secret,
        "secret unrelated memory",
    );
    for (id, content) in [
        (&visible, "pluggable retry plan scheduler"),
        (&other_ws, "another project"),
        (&secret, "secret unrelated memory"),
    ] {
        assert!(
            store
                .upsert_embedding(id, "test-v1", &[1.0, 0.0, 0.0], &hash(content))
                .unwrap()
        );
    }
    let result = store
        .search_hybrid(&query("ws1"), "test-v1", &[1.0, 0.0, 0.0])
        .unwrap();
    assert!(result.iter().any(|row| row.id == visible));
    assert!(
        !result
            .iter()
            .any(|row| row.id == other_ws || row.id == secret)
    );
    let lexical = store.recall(&query("ws1")).unwrap();
    assert!(!lexical.iter().any(|row| row.id == visible));
    let scoped = store
        .search_hybrid(&query("ws2"), "test-v1", &[1.0, 0.0, 0.0])
        .unwrap();
    assert!(!scoped.iter().any(|row| row.id == visible));
}
#[test]
fn rejects_bad_vectors_and_cleans_up_deleted_memories() {
    let db = store_path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    let id = put(
        &store,
        "ws",
        Sensitivity::Public,
        "correctable application state",
    );
    assert!(
        store
            .upsert_embedding(
                &id,
                "m1",
                &[f32::NAN, 0.0],
                &hash("correctable application state")
            )
            .is_err()
    );
    assert!(
        store
            .upsert_embedding(
                &id,
                "m1",
                &[0.0, 0.0],
                &hash("correctable application state")
            )
            .is_err()
    );
    assert!(
        store
            .upsert_embedding(
                &id,
                "m1",
                &[0.0, 1.0],
                &hash("correctable application state")
            )
            .unwrap()
    );
    assert!(
        !store
            .upsert_embedding(
                "missing",
                "m1",
                &[0.0, 1.0],
                &hash("correctable application state"),
            )
            .unwrap()
    );
    assert_eq!(store.clear_embeddings("m1").unwrap(), 1);
    assert!(
        store
            .upsert_embedding(
                &id,
                "m1",
                &[0.0, 1.0],
                &hash("correctable application state")
            )
            .unwrap()
    );
    assert!(store.forget(&id).unwrap());
    assert!(
        store
            .search_hybrid(&query("ws"), "m1", &[0.0, 1.0])
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.clear_embeddings("m1").unwrap(), 0);
}
#[test]
fn lexical_results_are_preserved_without_an_embedding() {
    let db = store_path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    let id = put(
        &store,
        "ws",
        Sensitivity::Public,
        "pluggable retry plan scheduler",
    );
    let mut q = query("ws");
    q.text = "retry scheduler".into();
    let lexical = store.recall(&q).unwrap();
    let hybrid = store.search_hybrid(&q, "none", &[]).unwrap();
    assert!(hybrid.iter().any(|memory| memory.id == id));
    assert_eq!(
        lexical.iter().map(|m| &m.id).collect::<Vec<_>>(),
        hybrid.iter().map(|m| &m.id).collect::<Vec<_>>()
    );
}

#[test]
fn stale_embedding_is_never_written_after_memory_content_changes() {
    let store = SqliteMemoryStore::open(&store_path()).unwrap();
    let id = put(
        &store,
        "ws",
        Sensitivity::Public,
        "newer revision of the memory",
    );
    assert!(
        !store
            .upsert_embedding(&id, "model", &[1.0, 0.0], &hash("outdated version"))
            .unwrap()
    );
    assert!(
        store
            .upsert_embedding(
                &id,
                "model",
                &[1.0, 0.0],
                &hash("newer revision of the memory")
            )
            .unwrap()
    );
    assert!(
        store
            .search_hybrid(&query("ws"), "model", &[1.0, 0.0])
            .unwrap()
            .iter()
            .any(|r| r.id == id)
    );
}

#[test]
fn backfill_retries_missing_vectors_and_never_crosses_workspace_scope() {
    let store = SqliteMemoryStore::open(&store_path()).unwrap();
    let id = put(&store, "ws", Sensitivity::Public, "backfill candidate");
    let _other = put(
        &store,
        "another-ws",
        Sensitivity::Public,
        "another workspace",
    );
    let _secret = put(
        &store,
        "ws",
        Sensitivity::Secret,
        "secret should not be embedded",
    );
    let pending = store
        .pending_embeddings("agent-cli", "ws", "model", 4)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, id);
    assert_eq!(pending[0].2, hash("backfill candidate"));
    assert!(
        store
            .upsert_embedding(&id, "model", &[1.0, 0.0], &pending[0].2)
            .unwrap()
    );
    assert!(
        store
            .pending_embeddings("agent-cli", "ws", "model", 4)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .pending_embeddings("agent-cli", "ws", "other-model", 4)
            .unwrap()
            .len(),
        1
    );
}
