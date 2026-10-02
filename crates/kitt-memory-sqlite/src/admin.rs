use super::*;
use kitt_memory_core::{DreamRunRecord, MemorySource};

fn map_dream(row: &rusqlite::Row<'_>) -> std::result::Result<DreamRunRecord, rusqlite::Error> {
    let as_u64 = |index: usize| -> std::result::Result<u64, rusqlite::Error> {
        let value = row.get::<_, i64>(index)?;
        if value < 0 {
            return Err(data_error(
                index,
                MemoryError::Corrupt("negative dream counter".into()),
            ));
        }
        Ok(value as u64)
    };
    Ok(DreamRunRecord {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        status: row.get(4)?,
        sessions_scanned: as_u64(5)?,
        entries_scanned: as_u64(6)?,
        signals_found: as_u64(7)?,
        memories_added: as_u64(8)?,
        memories_merged: as_u64(9)?,
        memories_superseded: as_u64(10)?,
        memories_archived: as_u64(11)?,
        model: row.get(12)?,
        input_tokens: as_u64(13)?,
        output_tokens: as_u64(14)?,
        failure_reason: row.get(15)?,
        dry_run: row.get::<_, i64>(16)? != 0,
    })
}

fn insert_dream_run(tx: &rusqlite::Transaction<'_>, run: &DreamRunRecord) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO dream_runs(id,workspace_id,started_at,finished_at,status,sessions_scanned,entries_scanned,signals_found,memories_added,memories_merged,memories_superseded,memories_archived,model,input_tokens,output_tokens,failure_reason,dry_run)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            run.id, run.workspace_id, run.started_at, run.finished_at, run.status,
            run.sessions_scanned as i64, run.entries_scanned as i64, run.signals_found as i64,
            run.memories_added as i64, run.memories_merged as i64,
            run.memories_superseded as i64, run.memories_archived as i64,
            run.model, run.input_tokens as i64, run.output_tokens as i64,
            run.failure_reason, run.dry_run as i64
        ],
    ).map_err(storage)?;
    Ok(())
}

impl SqliteMemoryStore {
    pub fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>> {
        self.with_conn(|conn| load_one(conn, id))
    }

    pub fn list_records(
        &self,
        namespace: &str,
        workspace_id: &str,
        status: Option<MemoryStatus>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        let limit = limit.clamp(1, 2048) as i64;
        self.with_conn(|conn| {
            if let Some(status) = status {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {MEMORY_COLUMNS} FROM memories m WHERE m.namespace=?1 AND m.workspace_id=?2 AND m.status=?3 ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT ?4"
                ))?;
                stmt.query_map(params![namespace, workspace_id, status.as_db(), limit], map_memory_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            } else {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {MEMORY_COLUMNS} FROM memories m WHERE m.namespace=?1 AND m.workspace_id=?2 ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT ?3"
                ))?;
                stmt.query_map(params![namespace, workspace_id, limit], map_memory_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            }
        })
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<bool> {
        let conn = self.writer_conn()?;
        Ok(conn
            .execute(
                "UPDATE memories SET pinned=?1,updated_at=?2 WHERE id=?3",
                params![pinned as i64, now_epoch(), id],
            )
            .map_err(storage)?
            > 0)
    }

    pub fn touch_records(&self, ids: &[String]) -> Result<()> {
        self.touch_access(ids.iter().map(String::as_str))
    }

    pub fn archive_workspace(
        &self,
        namespace: &str,
        workspace_id: &str,
    ) -> Result<Vec<MemoryRecord>> {
        let active =
            self.list_records(namespace, workspace_id, Some(MemoryStatus::Active), 2048)?;
        if active.is_empty() {
            return Ok(active);
        }
        let now = now_epoch();
        let conn = self.writer_conn()?;
        conn.execute(
            "UPDATE memories SET status='ARCHIVED',updated_at=?1,valid_until=CASE WHEN valid_until IS NULL OR valid_until>?1 THEN ?1 ELSE valid_until END WHERE namespace=?2 AND workspace_id=?3 AND status='ACTIVE'",
            params![now, namespace, workspace_id],
        ).map_err(storage)?;
        Ok(active)
    }

    pub fn last_dream_run(&self, workspace_id: &str) -> Result<Option<DreamRunRecord>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id,workspace_id,started_at,finished_at,status,sessions_scanned,entries_scanned,signals_found,memories_added,memories_merged,memories_superseded,memories_archived,model,input_tokens,output_tokens,failure_reason,dry_run
                 FROM dream_runs WHERE workspace_id=?1 AND status='COMPLETED' AND dry_run=0 ORDER BY COALESCE(finished_at,started_at) DESC LIMIT 1",
                [workspace_id],
                map_dream,
            ).optional()
        })
    }

    pub fn record_dream_run(&self, run: &DreamRunRecord) -> Result<()> {
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        insert_dream_run(&tx, run)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }

    pub fn commit_dream(
        &self,
        run: &DreamRunRecord,
        new_memories: &[MemoryRecord],
        updated_memories: &[MemoryRecord],
        sources: &[MemorySource],
    ) -> Result<()> {
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        for memory in updated_memories.iter().chain(new_memories.iter()) {
            let memory = memory.canonicalized_for_storage()?;
            upsert_record_tx(&tx, &memory)?;
        }
        for source in sources {
            tx.execute(
                "INSERT INTO memory_sources(id,memory_id,source_kind,source_id,source_uri,source_digest,relationship,source_revision,observed_at,valid_from,valid_until)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT DO UPDATE SET source_uri=excluded.source_uri,source_digest=excluded.source_digest,observed_at=excluded.observed_at,valid_from=excluded.valid_from,valid_until=excluded.valid_until",
                params![
                    source.id, source.memory_id, source.source_kind, source.source_id,
                    source.source_uri, source.source_digest, source.relationship,
                    source.source_revision, source.observed_at, source.valid_from, source.valid_until
                ],
            ).map_err(storage)?;
        }
        insert_dream_run(&tx, run)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }

    pub fn periodic_maintenance(&self) -> Result<()> {
        self.flush_recall_traces()?;
        self.prune_expired()?;
        let cutoff = now_epoch().saturating_sub(30 * 24 * 3600);
        let conn = self.writer_conn()?;
        conn.execute(
            "DELETE FROM memory_consumption_receipts WHERE consumed_at < ?1",
            [cutoff],
        )
        .map_err(storage)?;
        conn.execute(
            "DELETE FROM recall_traces WHERE created_at < ?1",
            [cutoff],
        )
        .map_err(storage)?;
        conn.execute(
            "DELETE FROM memory_jobs
             WHERE status IN ('SUCCEEDED','FAILED') AND updated_at < ?1",
            [cutoff],
        )
        .map_err(storage)?;
        conn.execute_batch(
            "INSERT INTO memories_fts(memories_fts) VALUES('optimize');
             PRAGMA optimize;
             PRAGMA wal_checkpoint(PASSIVE);",
        )
        .map_err(storage)?;
        Ok(())
    }

    pub fn maintenance(&self, namespace: &str, workspace_id: &str) -> Result<(usize, usize)> {
        self.periodic_maintenance()?;
        let duplicates = self.consolidate_exact_duplicates(namespace, workspace_id)?;
        Ok((0, duplicates))
    }
}
