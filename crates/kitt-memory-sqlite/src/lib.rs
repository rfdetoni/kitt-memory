use kitt_memory_core::{
    MemoryError, MemoryKind, MemoryRecord, MemoryScope, MemoryStatus, MemoryStore, NewMemory,
    RecallQuery, Result, Sensitivity, lexical_score, now_epoch,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

pub struct SqliteMemoryStore {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage)?;
        }
        let store = Self {
            path,
            write_lock: Mutex::new(()),
        };
        store.with_conn(migrate)?;
        Ok(store)
    }

    fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path).map_err(storage)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(storage)?;
        Ok(conn)
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> std::result::Result<T, rusqlite::Error>,
    ) -> Result<T> {
        let conn = self.conn()?;
        f(&conn).map_err(storage)
    }

    pub fn import_legacy_agent_db(&self, source: impl AsRef<Path>) -> Result<usize> {
        let source = Connection::open(source).map_err(storage)?;
        let mut stmt = source.prepare("SELECT id, workspace_id, kind, content, normalized_content, status, importance, confidence, created_at, updated_at, last_accessed_at, access_count, valid_until, supersedes_id, content_hash, pinned, COALESCE(metadata_json,'{}') FROM memories").map_err(storage)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MemoryRecord {
                    id: r.get(0)?,
                    namespace: "agent-cli".into(),
                    workspace_id: r.get(1)?,
                    kind: MemoryKind::from_db(&r.get::<_, String>(2)?),
                    content: r.get(3)?,
                    normalized_content: r.get(4)?,
                    status: MemoryStatus::from_db(&r.get::<_, String>(5)?),
                    sensitivity: Sensitivity::Private,
                    scope: MemoryScope::Workspace,
                    importance: r.get::<_, f64>(6)? as f32,
                    confidence: r.get::<_, f64>(7)? as f32,
                    created_at: r.get::<_, f64>(8)? as i64,
                    updated_at: r.get::<_, f64>(9)? as i64,
                    last_accessed_at: r.get::<_, Option<f64>>(10)?.map(|v| v as i64),
                    access_count: r.get::<_, i64>(11)? as u64,
                    valid_until: r.get::<_, Option<f64>>(12)?.map(|v| v as i64),
                    supersedes_id: r.get(13)?,
                    content_hash: r.get(14)?,
                    pinned: r.get::<_, i64>(15)? != 0,
                    metadata_json: r.get(16)?,
                })
            })
            .map_err(storage)?;
        let mut count = 0;
        let mut active_seen: HashMap<(String, String), String> = HashMap::new();
        for row in rows {
            let mut memory = row.map_err(storage)?;
            if memory.status == MemoryStatus::Active {
                let key = (memory.workspace_id.clone(), memory.content_hash.clone());
                if let Some(keeper) = active_seen.get(&key) {
                    memory.status = MemoryStatus::Superseded;
                    memory.supersedes_id = Some(keeper.clone());
                } else {
                    active_seen.insert(key, memory.id.clone());
                }
            }
            self.upsert_record(&memory)?;
            count += 1;
        }
        Ok(count)
    }
}

impl MemoryStore for SqliteMemoryStore {
    fn remember(&self, memory: NewMemory) -> Result<MemoryRecord> {
        if memory.sensitivity == Sensitivity::Ephemeral {
            return memory.into_record();
        }
        let record = memory.into_record()?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction().map_err(storage)?;

        let existing_id = tx.query_row(
            "SELECT id FROM memories WHERE namespace=?1 AND workspace_id=?2 AND content_hash=?3 AND status='ACTIVE' LIMIT 1",
            params![record.namespace, record.workspace_id, record.content_hash],
            |r| r.get::<_, String>(0),
        ).optional().map_err(storage)?;

        if let Some(existing_id) = existing_id {
            let existing = load_one(&tx, &existing_id)
                .map_err(storage)?
                .ok_or_else(|| MemoryError::Storage("duplicate lookup vanished".into()))?;
            let sensitivity = existing.sensitivity.most_restrictive(record.sensitivity);
            let pinned = existing.pinned || record.pinned;
            let importance = existing.importance.max(record.importance);
            let confidence = existing.confidence.max(record.confidence);
            let valid_until = merge_valid_until(existing.valid_until, record.valid_until);
            let updated_at = now_epoch();

            tx.execute(
                "UPDATE memories SET sensitivity=?1,pinned=?2,importance=?3,confidence=?4,valid_until=?5,updated_at=?6 WHERE id=?7",
                params![sensitivity.as_db(), pinned as i64, importance, confidence,
                    valid_until, updated_at, existing_id],
            ).map_err(storage)?;
            tx.commit().map_err(storage)?;

            return self
                .with_conn(|conn| load_one(conn, &existing_id))?
                .ok_or_else(|| MemoryError::Storage("merged duplicate vanished".into()));
        }

        insert_record(&tx, &record).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(record)
    }

    fn upsert_record(&self, m: &MemoryRecord) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
        self.with_conn(|conn| {
            let mut merged = m.clone();
            if let Some(existing) = load_one(conn, &m.id)? {
                // Sensitivity is a monotonic security property. Import/migrate
                // paths must never make an already stored record less strict.
                merged.sensitivity = existing.sensitivity.most_restrictive(m.sensitivity);
            }
            conn.execute(
                "INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,importance,confidence,created_at,updated_at,last_accessed_at,access_count,valid_until,supersedes_id,content_hash,pinned,metadata_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20) ON CONFLICT(id) DO UPDATE SET namespace=excluded.namespace,workspace_id=excluded.workspace_id,kind=excluded.kind,content=excluded.content,normalized_content=excluded.normalized_content,status=excluded.status,sensitivity=excluded.sensitivity,scope=excluded.scope,importance=excluded.importance,confidence=excluded.confidence,updated_at=excluded.updated_at,last_accessed_at=excluded.last_accessed_at,access_count=excluded.access_count,valid_until=excluded.valid_until,supersedes_id=excluded.supersedes_id,content_hash=excluded.content_hash,pinned=excluded.pinned,metadata_json=excluded.metadata_json",
                params![merged.id,merged.namespace,merged.workspace_id,merged.kind.as_db(),merged.content,merged.normalized_content,merged.status.as_db(),merged.sensitivity.as_db(),merged.scope.as_db(),merged.importance,merged.confidence,merged.created_at,merged.updated_at,merged.last_accessed_at,merged.access_count as i64,merged.valid_until,merged.supersedes_id,merged.content_hash,merged.pinned as i64,merged.metadata_json]
            )
        })?;
        Ok(())
    }

    fn recall(&self, q: &RecallQuery) -> Result<Vec<MemoryRecord>> {
        self.prune_expired()?;
        let mut candidates = self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM memories WHERE namespace=?1 AND (workspace_id=?2 OR scope='global') AND status='ACTIVE' ORDER BY pinned DESC,importance DESC,updated_at DESC LIMIT 128")?;
            let ids = stmt.query_map(params![q.namespace,q.workspace_id],
                |r| r.get::<_,String>(0))?
                .collect::<std::result::Result<Vec<_>,_>>()?;
            let mut out=Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(m)=load_one(conn,&id)? { out.push(m); }
            }
            Ok(out)
        })?;
        candidates.retain(|m| match m.sensitivity {
            Sensitivity::Secret => q.allow_secret,
            Sensitivity::Private => q.allow_private,
            Sensitivity::Ephemeral => false,
            _ => true,
        });
        let now = now_epoch();
        candidates.sort_by(|a, b| {
            lexical_score(&q.text, b, now)
                .partial_cmp(&lexical_score(&q.text, a, now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(q.limit.clamp(1, 50));
        if !candidates.is_empty() {
            let ids: Vec<_> = candidates.iter().map(|m| m.id.clone()).collect();
            let _guard = self
                .write_lock
                .lock()
                .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
            self.with_conn(|conn| {
                let tx=conn.unchecked_transaction()?;
                for id in &ids {
                    tx.execute("UPDATE memories SET last_accessed_at=?1,access_count=access_count+1 WHERE id=?2",
                        params![now,id])?;
                }
                tx.commit()?;
                Ok(())
            })?;
        }
        Ok(candidates)
    }

    fn forget(&self, id: &str) -> Result<bool> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
        Ok(self.with_conn(|c| c.execute("DELETE FROM memories WHERE id=?1", [id]))? > 0)
    }

    fn set_status(
        &self,
        id: &str,
        status: MemoryStatus,
        supersedes_id: Option<&str>,
    ) -> Result<bool> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
        Ok(self.with_conn(|c| {
            c.execute(
                "UPDATE memories SET status=?1,supersedes_id=?2,updated_at=?3 WHERE id=?4",
                params![status.as_db(), supersedes_id, now_epoch(), id],
            )
        })? > 0)
    }

    fn prune_expired(&self) -> Result<usize> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| MemoryError::Storage("memory write lock poisoned".into()))?;
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM memories WHERE valid_until IS NOT NULL AND valid_until <= ?1",
                [now_epoch()],
            )
        })
    }

    fn consolidate_exact_duplicates(&self, namespace: &str, workspace_id: &str) -> Result<usize> {
        let rows=self.with_conn(|conn|{
            let mut stmt=conn.prepare("SELECT content_hash,id,pinned,importance,updated_at FROM memories WHERE namespace=?1 AND workspace_id=?2 AND status='ACTIVE' ORDER BY pinned DESC,importance DESC,updated_at DESC")?;
            stmt.query_map(params![namespace,workspace_id],
                |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?
                .collect::<std::result::Result<Vec<_>,_>>()
        })?;
        let mut keeper: HashMap<String, String> = HashMap::new();
        let mut changed = 0;
        for (hash, id) in rows {
            if let Some(keep) = keeper.get(&hash) {
                if self.set_status(&id, MemoryStatus::Superseded, Some(keep))? {
                    changed += 1;
                }
            } else {
                keeper.insert(hash, id);
            }
        }
        Ok(changed)
    }
}

fn merge_valid_until(existing: Option<i64>, incoming: Option<i64>) -> Option<i64> {
    match (existing, incoming) {
        (None, _) | (_, None) => None,
        (Some(a), Some(b)) => Some(a.max(b)),
    }
}

fn insert_record(
    conn: &Connection,
    m: &MemoryRecord,
) -> std::result::Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,importance,confidence,created_at,updated_at,last_accessed_at,access_count,valid_until,supersedes_id,content_hash,pinned,metadata_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        params![m.id,m.namespace,m.workspace_id,m.kind.as_db(),m.content,m.normalized_content,m.status.as_db(),m.sensitivity.as_db(),m.scope.as_db(),m.importance,m.confidence,m.created_at,m.updated_at,m.last_accessed_at,m.access_count as i64,m.valid_until,m.supersedes_id,m.content_hash,m.pinned as i64,m.metadata_json])
}

fn load_one(
    conn: &Connection,
    id: &str,
) -> std::result::Result<Option<MemoryRecord>, rusqlite::Error> {
    conn.query_row(
        "SELECT id,namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,importance,confidence,created_at,updated_at,last_accessed_at,access_count,valid_until,supersedes_id,content_hash,pinned,metadata_json FROM memories WHERE id=?1",
        [id],
        |r|Ok(MemoryRecord{
            id:r.get(0)?,namespace:r.get(1)?,workspace_id:r.get(2)?,
            kind:MemoryKind::from_db(&r.get::<_,String>(3)?),content:r.get(4)?,
            normalized_content:r.get(5)?,
            status:MemoryStatus::from_db(&r.get::<_,String>(6)?),
            sensitivity:Sensitivity::from_db(&r.get::<_,String>(7)?),
            scope:MemoryScope::from_db(&r.get::<_,String>(8)?),
            importance:r.get::<_,f64>(9)? as f32,confidence:r.get::<_,f64>(10)? as f32,
            created_at:r.get(11)?,updated_at:r.get(12)?,last_accessed_at:r.get(13)?,
            access_count:r.get::<_,i64>(14)? as u64,valid_until:r.get(15)?,
            supersedes_id:r.get(16)?,content_hash:r.get(17)?,
            pinned:r.get::<_,i64>(18)?!=0,metadata_json:r.get(19)?
        })
    ).optional()
}

fn migrate(conn: &Connection) -> std::result::Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(r#"
    CREATE TABLE IF NOT EXISTS schema_info(version INTEGER NOT NULL);
    INSERT INTO schema_info(version) SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM schema_info);
    CREATE TABLE IF NOT EXISTS memories(
      id TEXT PRIMARY KEY, namespace TEXT NOT NULL, workspace_id TEXT NOT NULL, kind TEXT NOT NULL,
      content TEXT NOT NULL, normalized_content TEXT NOT NULL, status TEXT NOT NULL,
      sensitivity TEXT NOT NULL, scope TEXT NOT NULL, importance REAL NOT NULL, confidence REAL NOT NULL,
      created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, last_accessed_at INTEGER,
      access_count INTEGER NOT NULL DEFAULT 0, valid_until INTEGER, supersedes_id TEXT,
      content_hash TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0, metadata_json TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX IF NOT EXISTS idx_memories_lookup ON memories(namespace,workspace_id,status,pinned,importance);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_active_hash ON memories(namespace,workspace_id,content_hash) WHERE status='ACTIVE';
    "#)?;
    Ok(())
}
fn storage<E: std::fmt::Display>(e: E) -> MemoryError {
    MemoryError::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn memory(content: &str, sensitivity: Sensitivity, pinned: bool) -> NewMemory {
        NewMemory {
            namespace: "assistant".into(),
            workspace_id: "global".into(),
            kind: MemoryKind::UserPreference,
            content: content.into(),
            sensitivity,
            scope: MemoryScope::Global,
            importance: 0.8,
            confidence: 1.0,
            pinned,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        }
    }
    fn temp_db(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), now_epoch()))
    }
    #[test]
    fn remembers_and_recalls() {
        let path = temp_db("kitt-memory-test");
        let store = SqliteMemoryStore::open(&path).unwrap();
        store
            .remember(memory(
                "Prefere respostas curtas",
                Sensitivity::Private,
                false,
            ))
            .unwrap();
        let got = store
            .recall(&RecallQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                text: "respostas".into(),
                limit: 5,
                allow_private: true,
                allow_secret: false,
            })
            .unwrap();
        assert_eq!(1, got.len());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn duplicate_never_downgrades_sensitivity() {
        let path = temp_db("kitt-memory-dedupe");
        let store = SqliteMemoryStore::open(&path).unwrap();
        let first = store
            .remember(memory("same fact", Sensitivity::Public, false))
            .unwrap();
        let stricter = store
            .remember(memory("same   fact", Sensitivity::Secret, true))
            .unwrap();
        let weaker = store
            .remember(memory("same fact", Sensitivity::Personal, false))
            .unwrap();
        assert_eq!(first.id, stricter.id);
        assert_eq!(first.id, weaker.id);
        assert_eq!(Sensitivity::Secret, weaker.sensitivity);
        assert!(weaker.pinned);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn upsert_record_never_downgrades_sensitivity() {
        let path = temp_db("kitt-memory-upsert-sensitivity");
        let store = SqliteMemoryStore::open(&path).unwrap();
        let original = store
            .remember(memory("migration fact", Sensitivity::Secret, true))
            .unwrap();
        let mut imported = original.clone();
        imported.sensitivity = Sensitivity::Public;
        imported.content = "migration fact updated".into();
        store.upsert_record(&imported).unwrap();

        let rows = store
            .recall(&RecallQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                text: "migration fact".into(),
                limit: 5,
                allow_private: true,
                allow_secret: true,
            })
            .unwrap();
        let row = rows.iter().find(|row| row.id == original.id).unwrap();
        assert_eq!(Sensitivity::Secret, row.sensitivity);
        let _ = std::fs::remove_file(path);
    }
}
