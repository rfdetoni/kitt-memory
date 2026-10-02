use kitt_memory_sqlite::{RequestReceipt, SqliteMemoryStore};

#[test]
fn receipt_survives_reopen_and_conflicting_ids_cannot_repeat_writes() {
    let path = std::env::temp_dir().join(format!("kitt-receipt-{}.sqlite", uuid::Uuid::new_v4()));
    {
        let store = SqliteMemoryStore::open(&path).unwrap();
        assert!(matches!(
            store.begin_request("one", "digest").unwrap(),
            RequestReceipt::New
        ));
        assert!(matches!(
            store.begin_request("one", "other").unwrap(),
            RequestReceipt::Conflict
        ));
    }
    {
        let store = SqliteMemoryStore::open(&path).unwrap();
        assert!(matches!(
            store.begin_request("one", "digest").unwrap(),
            RequestReceipt::Pending
        ));
        store.finish_request("one", "{\"id\":\"saved\"}").unwrap();
    }
    {
        let store = SqliteMemoryStore::open(&path).unwrap();
        match store.begin_request("one", "digest").unwrap() {
            RequestReceipt::Completed(response) => assert_eq!(response, "{\"id\":\"saved\"}"),
            _ => panic!("lost durable response"),
        }
        assert_eq!(store.request_status("missing").unwrap(), None);
    }
    std::fs::remove_file(&path).unwrap();
}
