use kitt_memory_sqlite::SqliteMemoryStore;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: kitt-memory-migrate <agent-cli-history.db> <kitt-memory.db>");
        std::process::exit(2);
    }
    let store = SqliteMemoryStore::open(&args[2]).unwrap_or_else(|e| {
        eprintln!("open destination: {e}");
        std::process::exit(1)
    });
    match store.import_legacy_agent_db(&args[1]) {
        Ok(n) => println!("imported {n} memories"),
        Err(e) => {
            eprintln!("migration failed: {e}");
            std::process::exit(1)
        }
    }
}
