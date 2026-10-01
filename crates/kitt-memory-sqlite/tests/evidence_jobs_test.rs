use kitt_memory_core::{MemoryConsumptionReceipt, MemoryJob, RecallTrace, SemanticMemoryStore};
use kitt_memory_sqlite::SqliteMemoryStore;

fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "kitt-memory-evidence-{}-{}.sqlite3",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

#[test]
fn receipts_are_idempotent_and_monotonic() {
    let db = path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    store
        .record_recall_trace(&RecallTrace {
            id: "trace-1".into(),
            namespace: "agent-cli".into(),
            workspace_id: "ws".into(),
            query: "q".into(),
            planned_scopes_json: "{}".into(),
            candidates_json: "[]".into(),
            selected_json: "[]".into(),
            token_cost: 1,
            semantic_fallback: false,
            elapsed_us: 1,
            context_hash: "h".into(),
            created_at: 1,
        })
        .unwrap();
    let mut receipt = MemoryConsumptionReceipt {
        recall_trace_id: "trace-1".into(),
        memory_id: "mem-1".into(),
        consumer: "context-envelope".into(),
        purpose: "turn-context".into(),
        presented: true,
        referenced: false,
        used_for_action: false,
        outcome: "".into(),
        turn_id: "turn-1".into(),
        consumed_at: 2,
    };
    store.record_consumption_receipt(&receipt).unwrap();
    receipt.referenced = true;
    receipt.used_for_action = true;
    receipt.outcome = "validation-passed".into();
    receipt.consumed_at = 3;
    store.record_consumption_receipt(&receipt).unwrap();
    let rows = store.recent_consumption_receipts("ws", 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].referenced);
    assert!(rows[0].used_for_action);
    assert_eq!(rows[0].outcome, "validation-passed");
    let _ = std::fs::remove_file(db);
}

#[test]
fn jobs_are_deduplicated_and_leased() {
    let db = path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    let job = MemoryJob::new("extract", "session-1", "r1", "42", "digest").unwrap();
    let first = store.enqueue_memory_job(&job).unwrap();
    let duplicate = MemoryJob::new("extract", "session-1", "r1", "42", "digest").unwrap();
    let second = store.enqueue_memory_job(&duplicate).unwrap();
    assert_eq!(first.id, second.id);
    let claimed = store
        .claim_memory_job("extract", "worker-a", 60)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, first.id);
    assert_eq!(claimed.status, "RUNNING");
    assert!(
        store
            .complete_memory_job(&claimed.id, "worker-a", Some("out"))
            .unwrap()
    );
    assert!(
        store
            .claim_memory_job("extract", "worker-b", 60)
            .unwrap()
            .is_none()
    );
    let _ = std::fs::remove_file(db);
}


#[test]
fn failed_jobs_use_deterministic_retry_backoff() {
    let db = path();
    let store = SqliteMemoryStore::open(&db).unwrap();
    let job = MemoryJob::new("consolidate", "session-2", "r2", "84", "digest-2").unwrap();
    let enqueued = store.enqueue_memory_job(&job).unwrap();
    let claimed = store
        .claim_memory_job("consolidate", "worker-a", 60)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, enqueued.id);
    assert_eq!(claimed.attempt, 1);

    assert!(
        store
            .fail_memory_job(&claimed.id, "worker-a", None)
            .unwrap()
    );
    assert!(
        store
            .claim_memory_job("consolidate", "worker-b", 60)
            .unwrap()
            .is_none()
    );

    let _ = std::fs::remove_file(db);
}

#[test]
fn evidence_tiering_blocks_self_reinforcing_sources() {
    use kitt_memory_core::{assess_evidence, EvidenceOrigin};

    let user = assess_evidence(EvidenceOrigin::Human, "USER_CORRECTION");
    assert!(user.learnable);
    assert_eq!(user.priority, 100);

    for origin in [
        EvidenceOrigin::Memory,
        EvidenceOrigin::Skill,
        EvidenceOrigin::Plugin,
        EvidenceOrigin::Harness,
        EvidenceOrigin::System,
    ] {
        let assessment = assess_evidence(origin, "ENVIRONMENT_CONTEXT");
        assert!(!assessment.learnable);
    }
}
