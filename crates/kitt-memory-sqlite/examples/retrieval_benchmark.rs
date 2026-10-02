use kitt_memory_core::{MemoryKind, MemoryScope, MemoryStore, NewMemory, RecallQuery, Sensitivity};
use kitt_memory_sqlite::SqliteMemoryStore;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn percentile(samples: &mut [u128], numerator: usize, denominator: usize) -> u128 {
    samples.sort_unstable();
    if samples.is_empty() {
        return 0;
    }
    let index = samples
        .len()
        .saturating_mul(numerator)
        .div_ceil(denominator)
        .saturating_sub(1)
        .min(samples.len() - 1);
    samples[index]
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let records = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2_000)
        .clamp(100, 100_000);
    let iterations = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(250)
        .clamp(20, 10_000);

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!(
        "kitt-memory-benchmark-{}-{stamp}.sqlite3",
        std::process::id()
    ));
    let store = SqliteMemoryStore::open(&path)?;

    let seed_started = Instant::now();
    for index in 0..records {
        let category = index % 32;
        store.remember(NewMemory {
            namespace: "benchmark".into(),
            workspace_id: "workspace-a".into(),
            kind: if index % 5 == 0 {
                MemoryKind::ArchitectureDecision
            } else {
                MemoryKind::TechnicalFact
            },
            content: format!(
                "Project rule {category}: service component {index} uses deterministic retrieval and validates release gates"
            ),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.5 + (index % 5) as f32 * 0.1,
            confidence: 0.9,
            pinned: index % 97 == 0,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        })?;
    }

    let mut samples = Vec::with_capacity(iterations);
    let query = RecallQuery {
        namespace: "benchmark".into(),
        workspace_id: "workspace-a".into(),
        scope_key: None,
        text: String::new(),
        limit: 12,
        as_of: None,
        allow_private: true,
        allow_secret: false,
    };

    for iteration in 0..iterations + 20 {
        let mut request = query.clone();
        request.text = format!(
            "service component deterministic retrieval project rule {}",
            iteration % 32
        );
        let started = Instant::now();
        let rows = store.recall(&request)?;
        if rows.is_empty() {
            return Err("benchmark query unexpectedly returned no rows".into());
        }
        if iteration >= 20 {
            samples.push(started.elapsed().as_micros());
        }
    }

    let total_us: u128 = samples.iter().sum();
    let mean_us = total_us / samples.len().max(1) as u128;
    let mut p50_samples = samples.clone();
    let mut p95_samples = samples.clone();
    let mut p99_samples = samples.clone();
    let p50_us = percentile(&mut p50_samples, 50, 100);
    let p95_us = percentile(&mut p95_samples, 95, 100);
    let p99_us = percentile(&mut p99_samples, 99, 100);

    println!(
        "{{\"records\":{records},\"iterations\":{iterations},\"seed_ms\":{},\"mean_us\":{mean_us},\"p50_us\":{p50_us},\"p95_us\":{p95_us},\"p99_us\":{p99_us}}}",
        seed_started.elapsed().as_millis()
    );

    drop(store);
    cleanup(&path);
    Ok(())
}
