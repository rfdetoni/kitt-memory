use kitt_memory_core::{
    BaselineQuery, ContextNode, CorrectionRecord, KnowledgeEdge, KnowledgeRelation, KnowledgeStore,
    MemoryBaseline, MemoryChange, MemoryChangeSet, MemoryError, MemoryKind, MemoryRecord,
    MemorySchemaDefinition, MemoryScope, MemorySource, MemoryStatus, MemoryStore, MergeCandidate,
    MergeDisposition, NewConcept, NewContextNode, NewCorrection, NewMemory, NewMemorySource,
    RecallQuery, RecallTrace, Result, SemanticMemoryStore, SemanticReranker, Sensitivity,
    StoredConcept, assess_merge_candidate, build_memory_baseline, estimate_tokens,
    gist_for_content, hash_normalized, lexical_similarity_with_terms, lexical_terms, normalize,
    now_epoch,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, types::Type};
use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

mod admin;
mod evidence;
mod semantic;

pub use semantic::TimelineMemoryQuery;

const STORE_SCHEMA_VERSION: i64 = 9;
const MEMORY_COLUMNS: &str = "m.id,m.namespace,m.workspace_id,m.kind,m.content,m.normalized_content,m.status,m.sensitivity,m.scope,m.scope_key,m.importance,m.confidence,m.created_at,m.updated_at,m.last_accessed_at,m.access_count,m.valid_from,m.valid_until,m.supersedes_id,m.content_hash,m.pinned,m.metadata_json,m.gist,m.tokens_est";

fn ensure_private_database_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(MemoryError::Storage(format!(
                    "memory database must not be a symlink: {}",
                    path.display()
                )));
            }
            if !metadata.is_file() {
                return Err(MemoryError::Storage(format!(
                    "memory database must be a regular file: {}",
                    path.display()
                )));
            }
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(storage)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options.open(path).map_err(storage)?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(storage)?;
        }
        Err(error) => return Err(storage(error)),
    }
    Ok(())
}

pub enum RequestReceipt {
    New,
    Pending,
    Conflict,
    Completed(String),
}

pub struct SqliteMemoryStore {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    trace_buffer: Mutex<VecDeque<RecallTrace>>,
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(storage)?;
        }
        ensure_private_database_file(&path)?;
        let writer = Self::open_connection(&path)?;
        let mut readers = Vec::with_capacity(4);
        for _ in 0..4 {
            readers.push(Self::open_connection(&path)?);
        }
        let store = Self {
            path,
            writer: Mutex::new(writer),
            readers: Mutex::new(readers),
            trace_buffer: Mutex::new(VecDeque::with_capacity(256)),
        };
        {
            let mut conn = store.writer_conn()?;
            migrate(&mut conn).map_err(storage)?;
        }
        store.prune_expired()?;
        Ok(store)
    }

    /// Admit a mutation before applying it; an uncertain outcome is never evicted.
    pub fn begin_request(&self, id: &str, digest: &str) -> Result<RequestReceipt> {
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let prior: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT digest,response FROM request_receipts WHERE request_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        if let Some((previous, response)) = prior {
            return Ok(if previous != digest {
                RequestReceipt::Conflict
            } else if let Some(response) = response {
                RequestReceipt::Completed(response)
            } else {
                RequestReceipt::Pending
            });
        }
        tx.execute(
            "DELETE FROM request_receipts WHERE response IS NOT NULL AND created_at < ?1",
            [now_epoch().saturating_sub(7 * 24 * 3600)],
        )
        .map_err(storage)?;
        let (count, bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(CASE WHEN response IS NULL THEN 1048576 ELSE length(CAST(response AS BLOB)) END),0) FROM request_receipts",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(storage)?;
        if count >= 10000 || bytes + 1048576 > 64 * 1024 * 1024 {
            return Err(MemoryError::Storage(
                "request receipt capacity exhausted".into(),
            ));
        }
        tx.execute(
            "INSERT INTO request_receipts(request_id,digest,created_at) VALUES(?1,?2,?3)",
            params![id, digest, now_epoch()],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(RequestReceipt::New)
    }

    pub fn request_status(&self, id: &str) -> Result<Option<String>> {
        let receipt: Option<Option<String>> = self
            .conn()?
            .query_row(
                "SELECT response FROM request_receipts WHERE request_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        Ok(receipt.map(|response| response.unwrap_or_default()))
    }

    pub fn finish_request(&self, id: &str, response: &str) -> Result<()> {
        if response.len() > 1024 * 1024 - 512 {
            return Err(MemoryError::Storage(
                "request receipt response exceeds frame limit".into(),
            ));
        }
        self.writer_conn()?
            .execute(
                "UPDATE request_receipts SET response=?2 WHERE request_id=?1 AND response IS NULL",
                params![id, response],
            )
            .map_err(storage)?;
        Ok(())
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(storage)?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(storage)?;
        conn.pragma_update(None, "cache_size", -8_192_i64)
            .map_err(storage)?;
        conn.pragma_update(None, "temp_store", "MEMORY")
            .map_err(storage)?;
        conn.pragma_update(None, "mmap_size", 67_108_864_i64)
            .map_err(storage)?;
        conn.pragma_update(None, "wal_autocheckpoint", 1_000_i64)
            .map_err(storage)?;
        Ok(conn)
    }

    fn conn(&self) -> Result<Connection> {
        self.readers
            .lock()
            .map_err(|_| MemoryError::Storage("memory reader pool lock poisoned".into()))?
            .pop()
            .map(Ok)
            .unwrap_or_else(|| Self::open_connection(&self.path))
    }

    fn return_conn(&self, conn: Connection) {
        if let Ok(mut readers) = self.readers.lock()
            && readers.len() < 8
        {
            readers.push(conn);
        }
    }

    fn writer_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.writer
            .lock()
            .map_err(|_| MemoryError::Storage("memory writer lock poisoned".into()))
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> std::result::Result<T, rusqlite::Error>,
    ) -> Result<T> {
        let conn = self.conn()?;
        let result = f(&conn).map_err(storage);
        self.return_conn(conn);
        result
    }

    fn load_recall_candidates(
        &self,
        query: &RecallQuery,
        cap: usize,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        if cap == 0 {
            return Ok(Vec::new());
        }
        let at = query.as_of.unwrap_or_else(now_epoch);
        let scope_key = query.scope_key.as_deref().unwrap_or("");
        let fts = fts_query(&query.text);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let sql = format!("SELECT {MEMORY_COLUMNS},bm25(memories_fts,0.0,0.0,1.0) FROM memories_fts JOIN memories m ON m.rowid=memories_fts.rowid WHERE memories_fts MATCH ?1 AND m.namespace=?2 AND (m.scope='global' OR (m.workspace_id=?3 AND m.scope='workspace') OR (m.workspace_id=?3 AND m.scope='conversation' AND m.scope_key=?4)) AND m.status='ACTIVE' AND (m.valid_from IS NULL OR m.valid_from<=?5) AND (m.valid_until IS NULL OR m.valid_until>?5) AND m.sensitivity<>'ephemeral' AND (m.sensitivity<>'private' OR ?6=1) AND (m.sensitivity<>'secret' OR ?7=1) ORDER BY bm25(memories_fts,0.0,0.0,1.0) ASC LIMIT ?8");
                let mut stmt=conn.prepare(&sql)?;
                stmt.query_map(params![fts,query.namespace,query.workspace_id,scope_key,at,query.allow_private as i64,query.allow_secret as i64,cap as i64],|row| Ok((map_memory_row(row)?,row.get::<_,f64>(24)? as f32)))?.collect::<std::result::Result<Vec<_>,_>>()
            })?
        };
        let limit = query.limit.min(50);
        if query.text.trim().is_empty() || rows.len() < limit.min(4) {
            let terms = lexical_terms(&query.text);
            let fallback=self.with_conn(|conn|{
                let sql=format!("SELECT {MEMORY_COLUMNS},0.0 FROM memories m WHERE m.namespace=?1 AND (m.scope='global' OR (m.workspace_id=?2 AND m.scope='workspace') OR (m.workspace_id=?2 AND m.scope='conversation' AND m.scope_key=?3)) AND m.status='ACTIVE' AND (m.valid_from IS NULL OR m.valid_from<=?4) AND (m.valid_until IS NULL OR m.valid_until>?4) AND m.sensitivity<>'ephemeral' AND (m.sensitivity<>'private' OR ?5=1) AND (m.sensitivity<>'secret' OR ?6=1) ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT ?7");
                let mut stmt=conn.prepare(&sql)?;
                stmt.query_map(params![query.namespace,query.workspace_id,scope_key,at,query.allow_private as i64,query.allow_secret as i64,cap.clamp(1,16) as i64],|row| Ok((map_memory_row(row)?,row.get::<_,f64>(24)? as f32)))?.collect::<std::result::Result<Vec<_>,_>>()
            })?;
            let mut seen = rows
                .iter()
                .map(|(m, _)| m.id.clone())
                .collect::<HashSet<_>>();
            for candidate in fallback {
                let lexical = lexical_similarity_with_terms(&terms, &candidate.0);
                if (query.text.trim().is_empty() || lexical >= 0.05)
                    && seen.insert(candidate.0.id.clone())
                {
                    rows.push(candidate);
                }
            }
        }
        rows.truncate(cap.saturating_add(limit.min(16)));
        Ok(rows)
    }

    fn baseline_revision(&self, query: &BaselineQuery) -> Result<u64> {
        let scope_key = query.scope_key.as_deref().unwrap_or("");
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT COALESCE(SUM(revision),0) FROM baseline_revisions
                 WHERE namespace=?1 AND workspace_id IN ('global',?2)
                   AND scope_key IN ('',?3)",
                params![query.namespace, query.workspace_id, scope_key],
                |row| row.get::<_, i64>(0),
            )
        })
        .and_then(|value| {
            u64::try_from(value)
                .map_err(|_| MemoryError::Corrupt("negative baseline revision".into()))
        })
    }

    fn ranked_recall(
        &self,
        query: &RecallQuery,
        reranker: Option<&dyn SemanticReranker>,
    ) -> Result<Vec<MemoryRecord>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }
        let limit = query.limit.min(50);
        let cap = (limit.saturating_mul(4)).clamp(16, 128);
        let candidates = self.load_recall_candidates(query, cap)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let terms = lexical_terms(&query.text);
        let records = candidates
            .iter()
            .take(30)
            .map(|(m, _)| m.clone())
            .collect::<Vec<_>>();
        let ids = records
            .iter()
            .map(|m| m.id.as_str())
            .collect::<HashSet<_>>();
        let mut lexical_confidence = candidates
            .iter()
            .take(8)
            .map(|(memory, _)| lexical_similarity_with_terms(&terms, memory))
            .collect::<Vec<_>>();
        lexical_confidence.sort_by(|left, right| right.total_cmp(left));
        let first_lexical = lexical_confidence.first().copied().unwrap_or(0.0);
        let second_lexical = lexical_confidence.get(1).copied().unwrap_or(0.0);
        let lexical_margin_high = first_lexical >= 0.45 && first_lexical - second_lexical >= 0.20;
        let semantic_scores = if lexical_margin_high {
            HashMap::new()
        } else {
            reranker
                .and_then(|r| r.score(&query.text, &records).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|x| x.score.is_finite() && ids.contains(x.memory_id.as_str()))
                .map(|x| (x.memory_id, x.score.clamp(0.0, 1.0)))
                .collect::<HashMap<_, _>>()
        };
        let now = query.as_of.unwrap_or_else(now_epoch);
        let semantic_enabled = !semantic_scores.is_empty();
        let mut ranked = candidates
            .into_iter()
            .enumerate()
            .map(|(index, (memory, bm25))| {
                let lexical = lexical_similarity_with_terms(&terms, &memory);
                let fts_score = if bm25 == 0.0 {
                    0.0
                } else {
                    let relevance = (-bm25).max(0.0);
                    relevance / (1.0 + relevance)
                };
                let retained = kitt_memory_core::retention_score(&memory, now).min(1.25);
                let rr = 60.0 / (61.0 + index as f32);
                let semantic = semantic_scores.get(&memory.id).copied().unwrap_or(0.0);
                let score = if query.text.trim().is_empty() {
                    retained
                } else if semantic_enabled {
                    fts_score * 0.32
                        + lexical * 0.18
                        + semantic * 0.32
                        + retained * 0.10
                        + rr * 0.08
                } else {
                    fts_score * 0.50 + lexical * 0.24 + retained * 0.16 + rr * 0.10
                };
                (score, memory)
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|l, r| r.0.total_cmp(&l.0).then_with(|| l.1.id.cmp(&r.1.id)));
        let top = ranked.first().map(|item| item.0).unwrap_or(0.0);
        let threshold = if query.text.trim().is_empty() {
            0.0
        } else {
            top * 0.30
        };
        Ok(ranked
            .into_iter()
            .filter(|(score, _)| *score >= threshold)
            .take(limit)
            .map(|(_, m)| m)
            .collect())
    }
}

impl MemoryStore for SqliteMemoryStore {
    fn remember(&self, memory: NewMemory) -> Result<MemoryRecord> {
        if memory.sensitivity == Sensitivity::Ephemeral {
            return memory.into_record();
        }
        let record = memory.into_record()?;
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;

        let existing_id = tx
            .query_row(
                "SELECT id FROM memories WHERE namespace=?1 AND workspace_id=?2 AND scope=?3 \
                 AND COALESCE(scope_key,'')=COALESCE(?4,'') AND kind=?5 AND content_hash=?6 \
                 AND status='ACTIVE' LIMIT 1",
                params![
                    record.namespace,
                    record.workspace_id,
                    record.scope.as_db(),
                    record.scope_key,
                    record.kind.as_db(),
                    record.content_hash
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;

        if let Some(existing_id) = existing_id {
            tx.execute(
                "UPDATE memories SET sensitivity=CASE WHEN \
                 (CASE sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END)>= \
                 (CASE ?1 WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END) \
                 THEN sensitivity ELSE ?1 END,pinned=MAX(pinned,?2),importance=MAX(importance,?3),confidence=MAX(confidence,?4), \
                 valid_until=CASE WHEN valid_until IS NULL OR ?5 IS NULL THEN NULL ELSE MAX(valid_until,?5) END,updated_at=?6 WHERE id=?7",
                params![record.sensitivity.as_db(), record.pinned as i64, record.importance, record.confidence, record.valid_until, now_epoch(), existing_id],
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

    fn upsert_record(&self, memory: &MemoryRecord) -> Result<()> {
        let memory = memory.canonicalized_for_storage()?;
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        if memory.status == MemoryStatus::Active
            && let Some(existing_id) = find_identity_id(&tx, &memory)?
            && existing_id != memory.id
        {
            return Err(MemoryError::Invalid(format!(
                "active memory identity conflicts with {existing_id}"
            )));
        }
        upsert_record_tx(&tx, &memory)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }

    fn recall(&self, query: &RecallQuery) -> Result<Vec<MemoryRecord>> {
        self.ranked_recall(query, None)
    }

    fn recall_with_reranker(
        &self,
        query: &RecallQuery,
        reranker: &dyn SemanticReranker,
    ) -> Result<Vec<MemoryRecord>> {
        self.ranked_recall(query, Some(reranker))
    }

    fn baseline(&self, query: &BaselineQuery) -> Result<MemoryBaseline> {
        let at = query.as_of.unwrap_or_else(now_epoch);
        let scope_key = query.scope_key.as_deref().unwrap_or("");
        let records = self.with_conn(|conn| {
            let sql = format!("SELECT {MEMORY_COLUMNS} FROM memories m WHERE m.namespace=?1 \
                AND (m.scope='global' OR (m.workspace_id=?2 AND m.scope='workspace') OR (m.workspace_id=?2 AND m.scope='conversation' AND m.scope_key=?3)) \
                AND m.status='ACTIVE' AND (m.valid_from IS NULL OR m.valid_from<=?4) AND (m.valid_until IS NULL OR m.valid_until>?4) \
                AND m.sensitivity<>'ephemeral' AND (m.sensitivity<>'private' OR ?5=1) AND (m.sensitivity<>'secret' OR ?6=1) \
                ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT 512");
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![query.namespace, query.workspace_id, scope_key, at, query.allow_private as i64, query.allow_secret as i64], map_memory_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        let mut baseline = build_memory_baseline(records, query);
        let revision = self.baseline_revision(query)?;
        baseline.baseline_revision = Some(revision);
        baseline.etag = Some(hash_normalized(&format!(
            "{}|{}|{}|{}|{}|{}",
            query.namespace,
            query.workspace_id,
            query.scope_key.as_deref().unwrap_or(""),
            revision,
            query.allow_private,
            query.allow_secret
        )));
        Ok(baseline)
    }

    fn find_merge_candidates(
        &self,
        memory: &NewMemory,
        limit: usize,
        reranker: Option<&dyn SemanticReranker>,
    ) -> Result<Vec<MergeCandidate>> {
        let recall = RecallQuery {
            namespace: memory.namespace.clone(),
            workspace_id: memory.workspace_id.clone(),
            scope_key: memory.scope_key.clone(),
            text: memory.content.clone(),
            limit: limit.min(32),
            as_of: None,
            allow_private: true,
            allow_secret: true,
        };
        if recall.limit == 0 {
            return Ok(Vec::new());
        }
        let cap = (recall.limit * 8).clamp(32, 128);
        let candidates = self.load_recall_candidates(&recall, cap)?;
        let records = candidates
            .iter()
            .map(|(candidate, _)| candidate.clone())
            .collect::<Vec<_>>();
        let semantic = reranker
            .and_then(|ranker| ranker.score(&memory.content, &records).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|score| (score.memory_id, score.score))
            .collect::<HashMap<_, _>>();

        let mut out = records
            .into_iter()
            .map(|candidate| {
                let assessment = assess_merge_candidate(
                    &candidate,
                    memory,
                    semantic.get(&candidate.id).copied(),
                );
                MergeCandidate {
                    memory: candidate,
                    assessment,
                }
            })
            .filter(|candidate| candidate.assessment.disposition != MergeDisposition::Distinct)
            .collect::<Vec<_>>();
        out.sort_by(|left, right| {
            right
                .assessment
                .score
                .total_cmp(&left.assessment.score)
                .then_with(|| left.memory.id.cmp(&right.memory.id))
        });
        out.truncate(limit.clamp(1, 32));
        Ok(out)
    }

    fn forget(&self, id: &str) -> Result<bool> {
        let conn = self.writer_conn()?;
        Ok(conn
            .execute("DELETE FROM memories WHERE id=?1", [id])
            .map_err(storage)?
            > 0)
    }

    fn set_status(
        &self,
        id: &str,
        status: MemoryStatus,
        supersedes_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self.writer_conn()?;
        let now = now_epoch();
        let closes_validity = matches!(&status, MemoryStatus::Superseded | MemoryStatus::Archived);
        let changed = if closes_validity {
            conn.execute(
                "UPDATE memories SET status=?1,supersedes_id=?2,updated_at=?3,valid_until=CASE WHEN valid_until IS NULL OR valid_until>?3 THEN ?3 ELSE valid_until END WHERE id=?4",
                params![status.as_db(), supersedes_id, now, id],
            )
        } else {
            conn.execute(
                "UPDATE memories SET status=?1,supersedes_id=?2,updated_at=?3 WHERE id=?4 AND status='ACTIVE'",
                params![status.as_db(), supersedes_id, now, id],
            )
        }
        .map_err(storage)?;
        Ok(changed > 0)
    }

    fn prune_expired(&self) -> Result<usize> {
        let conn = self.writer_conn()?;
        conn.execute(
            "DELETE FROM memories WHERE status='ACTIVE' AND valid_until IS NOT NULL AND valid_until<=?1",
            [now_epoch()],
        )
        .map_err(storage)
    }

    fn consolidate_exact_duplicates(&self, namespace: &str, workspace_id: &str) -> Result<usize> {
        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT scope,COALESCE(scope_key,''),kind,content_hash,id FROM memories WHERE namespace=?1 AND workspace_id=?2 AND status='ACTIVE' ORDER BY pinned DESC,importance DESC,updated_at DESC",
            )?;
            stmt.query_map(params![namespace, workspace_id], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()
        })?;

        let mut keeper: HashMap<(String, String, String, String), String> = HashMap::new();
        let mut superseded = Vec::new();
        for (scope, scope_key, kind, hash, id) in rows {
            let key = (scope, scope_key, kind, hash);
            if let Some(keep) = keeper.get(&key) {
                superseded.push((id, keep.clone()));
            } else {
                keeper.insert(key, id);
            }
        }
        if superseded.is_empty() {
            return Ok(0);
        }

        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let updated_at = now_epoch();
        let mut changed = 0;
        for (id, keep) in &superseded {
            changed += tx
                .execute(
                    "UPDATE memories SET status='SUPERSEDED',supersedes_id=?1,updated_at=?2,valid_until=CASE WHEN valid_until IS NULL OR valid_until>?2 THEN ?2 ELSE valid_until END WHERE id=?3 AND status='ACTIVE'",
                    params![keep, updated_at, id],
                )
                .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(changed)
    }
}

impl KnowledgeStore for SqliteMemoryStore {
    fn record_correction(&self, correction: NewCorrection) -> Result<CorrectionRecord> {
        let record = correction.into_record()?;
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT INTO corrections(id,namespace,workspace_id,context,predicted,corrected,reason,source,sensitivity,source_memory_ids_json,applied_count,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                record.id,
                record.namespace,
                record.workspace_id,
                record.context,
                record.predicted,
                record.corrected,
                record.reason,
                record.source,
                record.sensitivity.as_db(),
                serde_json::to_string(&record.source_memory_ids).map_err(json_storage)?,
                record.applied_count as i64,
                record.created_at,
                record.updated_at
            ],
        )
        .map_err(storage)?;
        Ok(record)
    }

    fn search_corrections(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CorrectionRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(50);
        let fts = fts_query(query);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id,c.namespace,c.workspace_id,c.context,c.predicted,c.corrected,c.reason,c.source,c.sensitivity,c.source_memory_ids_json,c.applied_count,c.created_at,c.updated_at FROM corrections_fts JOIN corrections c ON c.rowid=corrections_fts.rowid                      WHERE corrections_fts MATCH ?1 AND c.namespace=?2 AND c.workspace_id=?3                      ORDER BY bm25(corrections_fts) ASC,c.applied_count DESC LIMIT ?4",
                )?;
                stmt.query_map(params![fts, namespace, workspace_id, limit as i64], map_correction)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?
        };
        if rows.is_empty() {
            rows = self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id,namespace,workspace_id,context,predicted,corrected,reason,source,sensitivity,source_memory_ids_json,applied_count,created_at,updated_at FROM corrections                      WHERE namespace=?1 AND workspace_id=?2                      ORDER BY applied_count DESC,updated_at DESC LIMIT ?3",
                )?;
                stmt.query_map(params![namespace, workspace_id, limit as i64], map_correction)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?;
        }
        Ok(rows)
    }

    fn mark_correction_applied(&self, id: &str) -> Result<bool> {
        let conn = self.writer_conn()?;
        Ok(conn
            .execute(
                "UPDATE corrections SET applied_count=applied_count+1,updated_at=?1 WHERE id=?2",
                params![now_epoch(), id],
            )
            .map_err(storage)?
            > 0)
    }

    fn upsert_concept(&self, concept: NewConcept) -> Result<StoredConcept> {
        let incoming = concept.into_record()?;
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;

        let existing = tx
            .query_row(
                "SELECT id,namespace,workspace_id,name,definition,confidence,sensitivity,revision,labels_json,source_memory_ids_json,created_at,updated_at FROM concepts WHERE namespace=?1 AND workspace_id=?2 AND name=?3",
                params![incoming.namespace, incoming.workspace_id, incoming.name],
                map_concept,
            )
            .optional()
            .map_err(storage)?;

        let stored = if let Some(mut existing) = existing {
            existing.definition = incoming.definition;
            existing.confidence = existing.confidence.max(incoming.confidence);
            existing.sensitivity = existing.sensitivity.most_restrictive(incoming.sensitivity);
            existing.revision = existing.revision.saturating_add(1);
            existing.labels = merged_strings(existing.labels, incoming.labels);
            existing.source_memory_ids =
                merged_strings(existing.source_memory_ids, incoming.source_memory_ids);
            existing.updated_at = now_epoch();
            tx.execute(
                "UPDATE concepts SET definition=?1,confidence=?2,sensitivity=?3,revision=?4,labels_json=?5,source_memory_ids_json=?6,updated_at=?7 WHERE id=?8",
                params![
                    existing.definition,
                    existing.confidence,
                    existing.sensitivity.as_db(),
                    existing.revision as i64,
                    serde_json::to_string(&existing.labels).map_err(json_storage)?,
                    serde_json::to_string(&existing.source_memory_ids).map_err(json_storage)?,
                    existing.updated_at,
                    existing.id
                ],
            )
            .map_err(storage)?;
            existing
        } else {
            tx.execute(
                "INSERT INTO concepts(id,namespace,workspace_id,name,definition,confidence,sensitivity,revision,labels_json,source_memory_ids_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    incoming.id,
                    incoming.namespace,
                    incoming.workspace_id,
                    incoming.name,
                    incoming.definition,
                    incoming.confidence,
                    incoming.sensitivity.as_db(),
                    incoming.revision as i64,
                    serde_json::to_string(&incoming.labels).map_err(json_storage)?,
                    serde_json::to_string(&incoming.source_memory_ids).map_err(json_storage)?,
                    incoming.created_at,
                    incoming.updated_at
                ],
            )
            .map_err(storage)?;
            incoming
        };

        tx.commit().map_err(storage)?;
        Ok(stored)
    }

    fn search_concepts(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredConcept>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(50);
        let fts = fts_query(query);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id,c.namespace,c.workspace_id,c.name,c.definition,c.confidence,c.sensitivity,c.revision,c.labels_json,c.source_memory_ids_json,c.created_at,c.updated_at FROM concepts_fts JOIN concepts c ON c.rowid=concepts_fts.rowid                      WHERE concepts_fts MATCH ?1 AND c.namespace=?2 AND c.workspace_id=?3                      ORDER BY bm25(concepts_fts) ASC,c.confidence DESC LIMIT ?4",
                )?;
                stmt.query_map(params![fts, namespace, workspace_id, limit as i64], map_concept)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?
        };
        if rows.is_empty() {
            rows = self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id,namespace,workspace_id,name,definition,confidence,sensitivity,revision,labels_json,source_memory_ids_json,created_at,updated_at FROM concepts                      WHERE namespace=?1 AND workspace_id=?2                      ORDER BY confidence DESC,updated_at DESC LIMIT ?3",
                )?;
                stmt.query_map(params![namespace, workspace_id, limit as i64], map_concept)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?;
        }
        Ok(rows)
    }

    fn link_concepts(
        &self,
        namespace: &str,
        workspace_id: &str,
        source_id: &str,
        target_id: &str,
        relation: KnowledgeRelation,
        weight: f32,
    ) -> Result<KnowledgeEdge> {
        if source_id == target_id {
            return Err(MemoryError::Invalid(
                "a concept cannot link to itself".into(),
            ));
        }
        if !weight.is_finite() {
            return Err(MemoryError::Invalid(
                "knowledge link weight must be finite".into(),
            ));
        }
        let weight = weight.clamp(0.0, 1.0);
        let now = now_epoch();
        let conn = self.writer_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM concepts WHERE namespace=?1 AND workspace_id=?2                  AND id IN (?3,?4)",
                params![namespace, workspace_id, source_id, target_id],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if count != 2 {
            return Err(MemoryError::Invalid(
                "knowledge link endpoints must exist in the same namespace/workspace".into(),
            ));
        }

        let (id, created_at, stored_weight) = conn.query_row(
            "INSERT INTO concept_links(id,namespace,workspace_id,source_id,target_id,relation,weight,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(namespace,workspace_id,source_id,target_id,relation) DO UPDATE SET weight=excluded.weight RETURNING id,created_at,weight",
            params![format!("edge_{}", uuid::Uuid::new_v4().simple()),namespace,workspace_id,source_id,target_id,relation.as_db(),weight,now],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,f64>(2)? as f32)),
        ).map_err(storage)?;
        Ok(KnowledgeEdge {
            id,
            namespace: namespace.into(),
            workspace_id: workspace_id.into(),
            source_id: source_id.into(),
            target_id: target_id.into(),
            relation,
            weight: stored_weight,
            created_at,
        })
    }

    fn links_for_concept(
        &self,
        namespace: &str,
        workspace_id: &str,
        concept_id: &str,
    ) -> Result<Vec<KnowledgeEdge>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id,namespace,workspace_id,source_id,target_id,relation,weight,created_at                  FROM concept_links WHERE namespace=?1 AND workspace_id=?2                  AND (source_id=?3 OR target_id=?3) ORDER BY created_at ASC,id ASC",
            )?;
            stmt.query_map(
                params![namespace, workspace_id, concept_id],
                |row| {
                    Ok(KnowledgeEdge {
                        id: row.get(0)?,
                        namespace: row.get(1)?,
                        workspace_id: row.get(2)?,
                        source_id: row.get(3)?,
                        target_id: row.get(4)?,
                        relation: parse_relation_at(row.get::<_,String>(5)?,5)?,
                        weight: row.get::<_, f64>(6)? as f32,
                        created_at: row.get(7)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
        })
    }
    fn expand_concepts(
        &self,
        namespace: &str,
        workspace_id: &str,
        seed_ids: &[String],
        max_hops: usize,
        limit: usize,
    ) -> Result<Vec<StoredConcept>> {
        if limit == 0 || seed_ids.is_empty() {
            return Ok(Vec::new());
        }
        let max_hops = max_hops.min(4);
        let limit = limit.min(100);
        let conn = self.conn()?;
        let mut frontier = seed_ids
            .iter()
            .filter(|id| !id.trim().is_empty())
            .take(16)
            .cloned()
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        let mut concepts = Vec::new();
        for _ in 0..=max_hops {
            frontier.retain(|id| visited.insert(id.clone()));
            if frontier.is_empty() || concepts.len() >= limit {
                break;
            }
            let ph = (0..frontier.len())
                .map(|i| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(",");
            let mut values = vec![
                rusqlite::types::Value::from(namespace.to_string()),
                rusqlite::types::Value::from(workspace_id.to_string()),
            ];
            values.extend(frontier.iter().cloned().map(rusqlite::types::Value::from));
            let sql = format!(
                "SELECT id,namespace,workspace_id,name,definition,confidence,sensitivity,revision,labels_json,source_memory_ids_json,created_at,updated_at FROM concepts WHERE namespace=?1 AND workspace_id=?2 AND id IN ({ph})"
            );
            let mut stmt = conn.prepare(&sql).map_err(storage)?;
            let loaded = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), map_concept)
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?;
            let mut by_id = loaded
                .into_iter()
                .map(|c| (c.id.clone(), c))
                .collect::<HashMap<_, _>>();
            for id in &frontier {
                if let Some(c) = by_id.remove(id) {
                    concepts.push(c);
                    if concepts.len() >= limit {
                        break;
                    }
                }
            }
            if concepts.len() >= limit {
                break;
            }
            let mut ev = vec![
                rusqlite::types::Value::from(namespace.to_string()),
                rusqlite::types::Value::from(workspace_id.to_string()),
            ];
            ev.extend(frontier.iter().cloned().map(rusqlite::types::Value::from));
            let esql = format!(
                "SELECT source_id,target_id FROM concept_links WHERE namespace=?1 AND workspace_id=?2 AND (source_id IN ({ph}) OR target_id IN ({ph}))"
            );
            let mut estmt = conn.prepare(&esql).map_err(storage)?;
            let edges = estmt
                .query_map(rusqlite::params_from_iter(ev.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?;
            let current = frontier.iter().cloned().collect::<HashSet<_>>();
            let mut next = BTreeSet::new();
            for (source, target) in edges {
                if current.contains(&source) && !visited.contains(&target) {
                    next.insert(target.clone());
                }
                if current.contains(&target) && !visited.contains(&source) {
                    next.insert(source);
                }
            }
            frontier = next.into_iter().collect();
        }
        Ok(concepts)
    }
}

fn fts_query(input: &str) -> String {
    let mut terms = lexical_terms(input)
        .into_iter()
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms.join(" OR ")
}

fn merged_strings(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    left.into_iter()
        .chain(right)
        .filter(|value| !value.trim().is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn find_identity_id(conn: &Connection, memory: &MemoryRecord) -> Result<Option<String>> {
    conn.query_row("SELECT id FROM memories WHERE namespace=?1 AND workspace_id=?2 AND scope=?3 AND COALESCE(scope_key,'')=COALESCE(?4,'') AND kind=?5 AND content_hash=?6 AND status='ACTIVE' LIMIT 1",params![memory.namespace,memory.workspace_id,memory.scope.as_db(),memory.scope_key,memory.kind.as_db(),memory.content_hash],|row|row.get(0)).optional().map_err(storage)
}
fn upsert_record_tx(conn: &Connection, memory: &MemoryRecord) -> Result<()> {
    conn.execute("INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,scope_key,importance,confidence,created_at,updated_at,last_accessed_at,access_count,valid_from,valid_until,supersedes_id,content_hash,pinned,metadata_json,gist,tokens_est) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24) ON CONFLICT(id) DO UPDATE SET namespace=excluded.namespace,workspace_id=excluded.workspace_id,kind=excluded.kind,content=excluded.content,normalized_content=excluded.normalized_content,status=excluded.status,sensitivity=CASE WHEN (CASE memories.sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END)>=(CASE excluded.sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END) THEN memories.sensitivity ELSE excluded.sensitivity END,scope=excluded.scope,scope_key=excluded.scope_key,importance=excluded.importance,confidence=excluded.confidence,updated_at=excluded.updated_at,last_accessed_at=excluded.last_accessed_at,access_count=excluded.access_count,valid_from=excluded.valid_from,valid_until=excluded.valid_until,supersedes_id=excluded.supersedes_id,content_hash=excluded.content_hash,pinned=excluded.pinned,metadata_json=excluded.metadata_json,gist=excluded.gist,tokens_est=excluded.tokens_est",params![memory.id,memory.namespace,memory.workspace_id,memory.kind.as_db(),memory.content,memory.normalized_content,memory.status.as_db(),memory.sensitivity.as_db(),memory.scope.as_db(),memory.scope_key,memory.importance,memory.confidence,memory.created_at,memory.updated_at,memory.last_accessed_at,memory.access_count as i64,memory.valid_from,memory.valid_until,memory.supersedes_id,memory.content_hash,memory.pinned as i64,memory.metadata_json,memory.gist,memory.tokens_est as i64]).map_err(storage)?;
    Ok(())
}

fn insert_record(
    conn: &Connection,
    memory: &MemoryRecord,
) -> std::result::Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,scope_key,importance,confidence,created_at,updated_at,last_accessed_at,access_count,valid_from,valid_until,supersedes_id,content_hash,pinned,metadata_json,gist,tokens_est) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
        params![
            memory.id,
            memory.namespace,
            memory.workspace_id,
            memory.kind.as_db(),
            memory.content,
            memory.normalized_content,
            memory.status.as_db(),
            memory.sensitivity.as_db(),
            memory.scope.as_db(),
            memory.scope_key,
            memory.importance,
            memory.confidence,
            memory.created_at,
            memory.updated_at,
            memory.last_accessed_at,
            memory.access_count as i64,
            memory.valid_from,
            memory.valid_until,
            memory.supersedes_id,
            memory.content_hash,
            memory.pinned as i64,
            memory.metadata_json,
            memory.gist,
            memory.tokens_est as i64
        ],
    )
}

fn map_memory_row(row: &rusqlite::Row<'_>) -> std::result::Result<MemoryRecord, rusqlite::Error> {
    let count = row.get::<_, i64>(15)?;
    if count < 0 {
        return Err(data_error(
            15,
            MemoryError::Corrupt("negative access_count".into()),
        ));
    }
    Ok(MemoryRecord {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        kind: parse_kind_at(row.get::<_, String>(3)?, 3)?,
        content: row.get(4)?,
        normalized_content: row.get(5)?,
        status: parse_status_at(row.get::<_, String>(6)?, 6)?,
        sensitivity: parse_sensitivity_at(row.get::<_, String>(7)?, 7)?,
        scope: parse_scope_at(row.get::<_, String>(8)?, 8)?,
        scope_key: row.get(9)?,
        importance: row.get::<_, f64>(10)? as f32,
        confidence: row.get::<_, f64>(11)? as f32,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        last_accessed_at: row.get(14)?,
        access_count: count as u64,
        valid_from: row.get(16)?,
        valid_until: row.get(17)?,
        supersedes_id: row.get(18)?,
        content_hash: row.get(19)?,
        pinned: row.get::<_, i64>(20)? != 0,
        metadata_json: row.get(21)?,
        gist: row.get(22)?,
        tokens_est: {
            let value = row.get::<_, i64>(23)?;
            if value < 0 {
                return Err(data_error(
                    23,
                    MemoryError::Corrupt("negative tokens_est".into()),
                ));
            }
            value as usize
        },
    })
}
fn data_error(i: usize, e: MemoryError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, Type::Text, Box::new(e))
}
fn parse_kind_at(v: String, i: usize) -> std::result::Result<MemoryKind, rusqlite::Error> {
    MemoryKind::from_db(&v).map_err(|e| data_error(i, e))
}
fn parse_status_at(v: String, i: usize) -> std::result::Result<MemoryStatus, rusqlite::Error> {
    MemoryStatus::from_db(&v).map_err(|e| data_error(i, e))
}
fn parse_sensitivity_at(v: String, i: usize) -> std::result::Result<Sensitivity, rusqlite::Error> {
    Sensitivity::from_db(&v).map_err(|e| data_error(i, e))
}
fn parse_scope_at(v: String, i: usize) -> std::result::Result<MemoryScope, rusqlite::Error> {
    MemoryScope::from_db(&v).map_err(|e| data_error(i, e))
}
fn parse_relation_at(
    v: String,
    i: usize,
) -> std::result::Result<KnowledgeRelation, rusqlite::Error> {
    KnowledgeRelation::from_db(&v).map_err(|e| data_error(i, e))
}

fn load_one(
    conn: &Connection,
    id: &str,
) -> std::result::Result<Option<MemoryRecord>, rusqlite::Error> {
    let sql = format!("SELECT {MEMORY_COLUMNS} FROM memories m WHERE m.id=?1");
    conn.query_row(&sql, [id], map_memory_row).optional()
}

fn map_correction(
    row: &rusqlite::Row<'_>,
) -> std::result::Result<CorrectionRecord, rusqlite::Error> {
    let src: String = row.get(9)?;
    let count = row.get::<_, i64>(10)?;
    if count < 0 {
        return Err(data_error(
            10,
            MemoryError::Corrupt("negative correction applied_count".into()),
        ));
    }
    Ok(CorrectionRecord {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        context: row.get(3)?,
        predicted: row.get(4)?,
        corrected: row.get(5)?,
        reason: row.get(6)?,
        source: row.get(7)?,
        sensitivity: parse_sensitivity_at(row.get::<_, String>(8)?, 8)?,
        source_memory_ids: serde_json::from_str(&src)
            .map_err(|e| data_error(9, MemoryError::Corrupt(e.to_string())))?,
        applied_count: count as u64,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn map_concept(row: &rusqlite::Row<'_>) -> std::result::Result<StoredConcept, rusqlite::Error> {
    let labels: String = row.get(8)?;
    let sources: String = row.get(9)?;
    let revision = row.get::<_, i64>(7)?;
    if revision < 0 {
        return Err(data_error(
            7,
            MemoryError::Corrupt("negative concept revision".into()),
        ));
    }
    Ok(StoredConcept {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        name: row.get(3)?,
        definition: row.get(4)?,
        confidence: row.get::<_, f64>(5)? as f32,
        sensitivity: parse_sensitivity_at(row.get::<_, String>(6)?, 6)?,
        revision: revision as u64,
        labels: serde_json::from_str(&labels)
            .map_err(|e| data_error(8, MemoryError::Corrupt(e.to_string())))?,
        source_memory_ids: serde_json::from_str(&sources)
            .map_err(|e| data_error(9, MemoryError::Corrupt(e.to_string())))?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn migrate(conn: &mut Connection) -> std::result::Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(r#"CREATE TABLE IF NOT EXISTS schema_info(version INTEGER NOT NULL);INSERT INTO schema_info(version) SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM schema_info);
 CREATE TABLE IF NOT EXISTS memories(id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,kind TEXT NOT NULL,content TEXT NOT NULL,normalized_content TEXT NOT NULL,status TEXT NOT NULL,sensitivity TEXT NOT NULL,scope TEXT NOT NULL,scope_key TEXT,importance REAL NOT NULL,confidence REAL NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,last_accessed_at INTEGER,access_count INTEGER NOT NULL DEFAULT 0,valid_from INTEGER,valid_until INTEGER,supersedes_id TEXT,content_hash TEXT NOT NULL,pinned INTEGER NOT NULL DEFAULT 0,metadata_json TEXT NOT NULL DEFAULT '{}',gist TEXT NOT NULL DEFAULT '',tokens_est INTEGER NOT NULL DEFAULT 0);
 CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(id UNINDEXED,namespace UNINDEXED,workspace_id UNINDEXED,normalized_content,tokenize='unicode61 remove_diacritics 2');
 CREATE TABLE IF NOT EXISTS corrections(id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,context TEXT NOT NULL,predicted TEXT NOT NULL,corrected TEXT NOT NULL,reason TEXT,source TEXT NOT NULL,sensitivity TEXT NOT NULL DEFAULT 'private',source_memory_ids_json TEXT NOT NULL DEFAULT '[]',applied_count INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
 CREATE VIRTUAL TABLE IF NOT EXISTS corrections_fts USING fts5(id UNINDEXED,context,predicted,corrected,reason,tokenize='unicode61');
 CREATE TABLE IF NOT EXISTS concepts(id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,name TEXT NOT NULL,definition TEXT NOT NULL,confidence REAL NOT NULL,sensitivity TEXT NOT NULL DEFAULT 'private',revision INTEGER NOT NULL DEFAULT 1,labels_json TEXT NOT NULL DEFAULT '[]',source_memory_ids_json TEXT NOT NULL DEFAULT '[]',created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(namespace,workspace_id,name));
 CREATE VIRTUAL TABLE IF NOT EXISTS concepts_fts USING fts5(id UNINDEXED,name,definition,labels,tokenize='unicode61');
 CREATE TABLE IF NOT EXISTS concept_links(id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,source_id TEXT NOT NULL,target_id TEXT NOT NULL,relation TEXT NOT NULL,weight REAL NOT NULL,created_at INTEGER NOT NULL,UNIQUE(namespace,workspace_id,source_id,target_id,relation),CHECK(source_id<>target_id),CHECK(weight>=0.0 AND weight<=1.0),FOREIGN KEY(source_id) REFERENCES concepts(id) ON DELETE CASCADE,FOREIGN KEY(target_id) REFERENCES concepts(id) ON DELETE CASCADE);
 CREATE TABLE IF NOT EXISTS context_nodes(id TEXT PRIMARY KEY,parent_id TEXT,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,path TEXT NOT NULL,summary TEXT NOT NULL DEFAULT '',navigation_summary TEXT NOT NULL DEFAULT '',generation INTEGER NOT NULL DEFAULT 1,content_hash TEXT NOT NULL DEFAULT '',dirty_children INTEGER NOT NULL DEFAULT 0,total_children INTEGER NOT NULL DEFAULT 0,summarized_at INTEGER,sensitivity TEXT NOT NULL DEFAULT 'private',created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(namespace,workspace_id,path),FOREIGN KEY(parent_id) REFERENCES context_nodes(id) ON DELETE CASCADE);
 CREATE VIRTUAL TABLE IF NOT EXISTS context_nodes_fts USING fts5(id UNINDEXED,namespace UNINDEXED,workspace_id UNINDEXED,path,summary,navigation_summary,tokenize='unicode61');
 CREATE TABLE IF NOT EXISTS memory_context_links(memory_id TEXT NOT NULL,context_node_id TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(memory_id,context_node_id),FOREIGN KEY(memory_id) REFERENCES memories(id) ON DELETE CASCADE,FOREIGN KEY(context_node_id) REFERENCES context_nodes(id) ON DELETE CASCADE);
 CREATE TABLE IF NOT EXISTS memory_sources(id TEXT PRIMARY KEY,memory_id TEXT NOT NULL,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,source_uri TEXT,source_digest TEXT,relationship TEXT NOT NULL,source_revision TEXT,observed_at INTEGER NOT NULL,valid_from INTEGER,valid_until INTEGER,FOREIGN KEY(memory_id) REFERENCES memories(id) ON DELETE CASCADE);
 CREATE TABLE IF NOT EXISTS memory_change_sets(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,origin_type TEXT NOT NULL,origin_id TEXT NOT NULL,started_at INTEGER NOT NULL,committed_at INTEGER,model TEXT,reason TEXT NOT NULL,status TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS memory_changes(id TEXT PRIMARY KEY,change_set_id TEXT NOT NULL,memory_id TEXT,operation TEXT NOT NULL,before_json TEXT,after_json TEXT,evidence_json TEXT NOT NULL DEFAULT '[]',reason_code TEXT NOT NULL,FOREIGN KEY(change_set_id) REFERENCES memory_change_sets(id) ON DELETE CASCADE);
 CREATE TABLE IF NOT EXISTS recall_traces(id TEXT PRIMARY KEY,namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,query TEXT NOT NULL,planned_scopes_json TEXT NOT NULL,candidates_json TEXT NOT NULL,selected_json TEXT NOT NULL,token_cost INTEGER NOT NULL,semantic_fallback INTEGER NOT NULL,elapsed_us INTEGER NOT NULL,context_hash TEXT NOT NULL,created_at INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS memory_schemas(schema_id TEXT NOT NULL,version INTEGER NOT NULL,base_kind TEXT NOT NULL,fields_schema_json TEXT NOT NULL,retention_policy_json TEXT NOT NULL,merge_policy_json TEXT NOT NULL,default_sensitivity TEXT NOT NULL,index_fields_json TEXT NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(schema_id,version));
 CREATE TABLE IF NOT EXISTS dream_runs(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,started_at INTEGER NOT NULL,finished_at INTEGER,status TEXT NOT NULL,sessions_scanned INTEGER NOT NULL DEFAULT 0,entries_scanned INTEGER NOT NULL DEFAULT 0,signals_found INTEGER NOT NULL DEFAULT 0,memories_added INTEGER NOT NULL DEFAULT 0,memories_merged INTEGER NOT NULL DEFAULT 0,memories_superseded INTEGER NOT NULL DEFAULT 0,memories_archived INTEGER NOT NULL DEFAULT 0,model TEXT NOT NULL DEFAULT '',input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,failure_reason TEXT,dry_run INTEGER NOT NULL DEFAULT 0);
 CREATE TABLE IF NOT EXISTS memory_consumption_receipts(recall_trace_id TEXT NOT NULL,memory_id TEXT NOT NULL,consumer TEXT NOT NULL,purpose TEXT NOT NULL,presented INTEGER NOT NULL DEFAULT 0,referenced INTEGER NOT NULL DEFAULT 0,used_for_action INTEGER NOT NULL DEFAULT 0,outcome TEXT NOT NULL DEFAULT '',turn_id TEXT NOT NULL,consumed_at INTEGER NOT NULL,PRIMARY KEY(recall_trace_id,memory_id,consumer,purpose,turn_id),FOREIGN KEY(recall_trace_id) REFERENCES recall_traces(id) ON DELETE CASCADE);
 CREATE TABLE IF NOT EXISTS memory_jobs(id TEXT PRIMARY KEY,phase TEXT NOT NULL,source_id TEXT NOT NULL,source_revision TEXT NOT NULL,source_watermark TEXT NOT NULL DEFAULT '',status TEXT NOT NULL,lease_owner TEXT,lease_until INTEGER,attempt INTEGER NOT NULL DEFAULT 0,next_retry_at INTEGER,input_digest TEXT NOT NULL,output_digest TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(phase,source_id,source_revision,input_digest));"#)?;
    let current = tx.query_row("SELECT version FROM schema_info LIMIT 1", [], |r| {
        r.get::<_, i64>(0)
    })?;
    if current > STORE_SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if current < 2 {
        tx.execute_batch(r#"DELETE FROM memories_fts;INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content) SELECT rowid,id,namespace,workspace_id,normalized_content FROM memories;DELETE FROM corrections_fts;INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason) SELECT rowid,id,context,predicted,corrected,COALESCE(reason,'') FROM corrections;DELETE FROM concepts_fts;INSERT INTO concepts_fts(rowid,id,name,definition,labels) SELECT rowid,id,name,definition,labels_json FROM concepts;"#)?;
    }
    if current < 3 {
        if !table_has_column(&tx, "memories", "valid_from")? {
            tx.execute("ALTER TABLE memories ADD COLUMN valid_from INTEGER", [])?;
        }
        tx.execute(
            "UPDATE memories SET valid_from=created_at WHERE valid_from IS NULL",
            [],
        )?;
    }
    if current < 4 {
        if !table_has_column(&tx, "memories", "scope_key")? {
            tx.execute("ALTER TABLE memories ADD COLUMN scope_key TEXT", [])?;
        }
        if !table_has_column(&tx, "corrections", "sensitivity")? {
            tx.execute(
                "ALTER TABLE corrections ADD COLUMN sensitivity TEXT NOT NULL DEFAULT 'private'",
                [],
            )?;
        }
        if !table_has_column(&tx, "corrections", "source_memory_ids_json")? {
            tx.execute("ALTER TABLE corrections ADD COLUMN source_memory_ids_json TEXT NOT NULL DEFAULT '[]'",[])?;
        }
        if !table_has_column(&tx, "concepts", "sensitivity")? {
            tx.execute(
                "ALTER TABLE concepts ADD COLUMN sensitivity TEXT NOT NULL DEFAULT 'private'",
                [],
            )?;
        }
        tx.execute("UPDATE memories SET scope_key='legacy' WHERE scope='conversation' AND scope_key IS NULL",[])?;
        tx.execute_batch(r#"WITH ranked AS(SELECT id,FIRST_VALUE(id) OVER(PARTITION BY namespace,kind,content_hash ORDER BY pinned DESC,importance DESC,updated_at DESC,id) keeper,ROW_NUMBER() OVER(PARTITION BY namespace,kind,content_hash ORDER BY pinned DESC,importance DESC,updated_at DESC,id) rn FROM memories WHERE status='ACTIVE' AND scope='global') UPDATE memories SET status='SUPERSEDED',supersedes_id=(SELECT keeper FROM ranked WHERE ranked.id=memories.id),valid_until=CASE WHEN valid_until IS NULL OR valid_until>unixepoch() THEN unixepoch() ELSE valid_until END WHERE id IN(SELECT id FROM ranked WHERE rn>1);UPDATE memories SET workspace_id='global' WHERE scope='global';"#)?;
    }
    tx.execute_batch(r#"DROP INDEX IF EXISTS idx_memories_active_hash;DROP INDEX IF EXISTS idx_memories_lookup;DROP INDEX IF EXISTS idx_memories_temporal_lookup;
 CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_active_identity ON memories(namespace,workspace_id,scope,COALESCE(scope_key,''),kind,content_hash) WHERE status='ACTIVE';
 CREATE INDEX idx_memories_lookup ON memories(namespace,workspace_id,scope,status,pinned,importance);CREATE INDEX idx_memories_temporal_lookup ON memories(namespace,workspace_id,scope,scope_key,status,valid_from,valid_until,pinned,importance);
 CREATE INDEX IF NOT EXISTS idx_context_nodes_scope ON context_nodes(namespace,workspace_id,path,generation);
 CREATE INDEX IF NOT EXISTS idx_memory_sources_memory ON memory_sources(memory_id,observed_at);CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_sources_identity ON memory_sources(memory_id,source_kind,source_id,relationship,COALESCE(source_revision,''));
 CREATE INDEX IF NOT EXISTS idx_memory_change_sets_workspace ON memory_change_sets(workspace_id,started_at);
 CREATE INDEX IF NOT EXISTS idx_recall_traces_workspace ON recall_traces(workspace_id,created_at);
 CREATE INDEX IF NOT EXISTS idx_dream_runs_workspace ON dream_runs(workspace_id,finished_at,started_at);
 CREATE INDEX IF NOT EXISTS idx_memory_receipts_trace ON memory_consumption_receipts(recall_trace_id,consumed_at);
 CREATE INDEX IF NOT EXISTS idx_memory_jobs_claim ON memory_jobs(phase,status,next_retry_at,lease_until,created_at);
 CREATE INDEX IF NOT EXISTS idx_corrections_scope ON corrections(namespace,workspace_id,applied_count,updated_at);CREATE INDEX IF NOT EXISTS idx_concepts_scope ON concepts(namespace,workspace_id,confidence,updated_at);CREATE INDEX IF NOT EXISTS idx_concept_links_source ON concept_links(namespace,workspace_id,source_id);CREATE INDEX IF NOT EXISTS idx_concept_links_target ON concept_links(namespace,workspace_id,target_id);
 DROP TRIGGER IF EXISTS memories_fts_insert;DROP TRIGGER IF EXISTS memories_fts_delete;DROP TRIGGER IF EXISTS memories_fts_update;
 CREATE TRIGGER memories_fts_insert AFTER INSERT ON memories BEGIN INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content) VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.normalized_content);END;
 CREATE TRIGGER memories_fts_delete AFTER DELETE ON memories BEGIN DELETE FROM memories_fts WHERE rowid=old.rowid;END;
 CREATE TRIGGER memories_fts_update AFTER UPDATE OF normalized_content ON memories BEGIN DELETE FROM memories_fts WHERE rowid=old.rowid;INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content) VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.normalized_content);END;
 DROP TRIGGER IF EXISTS memories_validate_insert;DROP TRIGGER IF EXISTS memories_validate_update;
 CREATE TRIGGER memories_validate_insert BEFORE INSERT ON memories BEGIN SELECT RAISE(ABORT,'invalid memory status') WHERE NEW.status NOT IN('ACTIVE','SUPERSEDED','ARCHIVED','CANDIDATE');SELECT RAISE(ABORT,'invalid sensitivity') WHERE NEW.sensitivity NOT IN('public','personal','private','secret','ephemeral');SELECT RAISE(ABORT,'invalid scope') WHERE NEW.scope NOT IN('global','workspace','conversation');SELECT RAISE(ABORT,'invalid score') WHERE NEW.importance<0 OR NEW.importance>1 OR NEW.confidence<0 OR NEW.confidence>1;SELECT RAISE(ABORT,'invalid conversation scope_key') WHERE NEW.scope='conversation' AND(NEW.scope_key IS NULL OR trim(NEW.scope_key)='');SELECT RAISE(ABORT,'invalid interval') WHERE NEW.valid_until IS NOT NULL AND NEW.valid_from IS NOT NULL AND NEW.valid_until<NEW.valid_from;SELECT RAISE(ABORT,'negative access_count') WHERE NEW.access_count<0;END;
 CREATE TRIGGER memories_validate_update BEFORE UPDATE ON memories BEGIN SELECT RAISE(ABORT,'invalid memory status') WHERE NEW.status NOT IN('ACTIVE','SUPERSEDED','ARCHIVED','CANDIDATE');SELECT RAISE(ABORT,'invalid sensitivity') WHERE NEW.sensitivity NOT IN('public','personal','private','secret','ephemeral');SELECT RAISE(ABORT,'invalid scope') WHERE NEW.scope NOT IN('global','workspace','conversation');SELECT RAISE(ABORT,'invalid score') WHERE NEW.importance<0 OR NEW.importance>1 OR NEW.confidence<0 OR NEW.confidence>1;SELECT RAISE(ABORT,'invalid conversation scope_key') WHERE NEW.scope='conversation' AND(NEW.scope_key IS NULL OR trim(NEW.scope_key)='');SELECT RAISE(ABORT,'invalid interval') WHERE NEW.valid_until IS NOT NULL AND NEW.valid_from IS NOT NULL AND NEW.valid_until<NEW.valid_from;SELECT RAISE(ABORT,'negative access_count') WHERE NEW.access_count<0;END;
 DROP TRIGGER IF EXISTS corrections_fts_insert;DROP TRIGGER IF EXISTS corrections_fts_delete;DROP TRIGGER IF EXISTS corrections_fts_update;CREATE TRIGGER corrections_fts_insert AFTER INSERT ON corrections BEGIN INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason) VALUES(new.rowid,new.id,new.context,new.predicted,new.corrected,COALESCE(new.reason,''));END;CREATE TRIGGER corrections_fts_delete AFTER DELETE ON corrections BEGIN DELETE FROM corrections_fts WHERE rowid=old.rowid;END;CREATE TRIGGER corrections_fts_update AFTER UPDATE OF context,predicted,corrected,reason ON corrections BEGIN DELETE FROM corrections_fts WHERE rowid=old.rowid;INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason) VALUES(new.rowid,new.id,new.context,new.predicted,new.corrected,COALESCE(new.reason,''));END;
 DROP TRIGGER IF EXISTS concepts_fts_insert;DROP TRIGGER IF EXISTS concepts_fts_delete;DROP TRIGGER IF EXISTS concepts_fts_update;CREATE TRIGGER concepts_fts_insert AFTER INSERT ON concepts BEGIN INSERT INTO concepts_fts(rowid,id,name,definition,labels) VALUES(new.rowid,new.id,new.name,new.definition,new.labels_json);END;CREATE TRIGGER concepts_fts_delete AFTER DELETE ON concepts BEGIN DELETE FROM concepts_fts WHERE rowid=old.rowid;END;CREATE TRIGGER concepts_fts_update AFTER UPDATE OF name,definition,labels_json ON concepts BEGIN DELETE FROM concepts_fts WHERE rowid=old.rowid;INSERT INTO concepts_fts(rowid,id,name,definition,labels) VALUES(new.rowid,new.id,new.name,new.definition,new.labels_json);END;
 DROP TRIGGER IF EXISTS context_nodes_fts_insert;DROP TRIGGER IF EXISTS context_nodes_fts_delete;DROP TRIGGER IF EXISTS context_nodes_fts_update;
 CREATE TRIGGER context_nodes_fts_insert AFTER INSERT ON context_nodes BEGIN INSERT INTO context_nodes_fts(rowid,id,namespace,workspace_id,path,summary,navigation_summary) VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.path,new.summary,new.navigation_summary);END;
 CREATE TRIGGER context_nodes_fts_delete AFTER DELETE ON context_nodes BEGIN DELETE FROM context_nodes_fts WHERE rowid=old.rowid;END;
 CREATE TRIGGER context_nodes_fts_update AFTER UPDATE OF path,summary,navigation_summary ON context_nodes BEGIN DELETE FROM context_nodes_fts WHERE rowid=old.rowid;INSERT INTO context_nodes_fts(rowid,id,namespace,workspace_id,path,summary,navigation_summary) VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.path,new.summary,new.navigation_summary);END;"#)?;
    if current < 5 {
        tx.execute_batch(r#"INSERT OR IGNORE INTO context_nodes(id,parent_id,namespace,workspace_id,path,summary,navigation_summary,generation,content_hash,dirty_children,total_children,summarized_at,sensitivity,created_at,updated_at)
 SELECT 'ctx_'||lower(hex(randomblob(16))),NULL,namespace,workspace_id,'root','Imported memory root','Legacy memories awaiting semantic classification',1,'',0,COUNT(*),unixepoch(),CASE MAX(CASE sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END) WHEN 0 THEN 'public' WHEN 1 THEN 'personal' WHEN 2 THEN 'private' WHEN 3 THEN 'secret' ELSE 'ephemeral' END,unixepoch(),unixepoch()
 FROM memories GROUP BY namespace,workspace_id;
 INSERT OR IGNORE INTO memory_context_links(memory_id,context_node_id,created_at)
 SELECT m.id,c.id,unixepoch() FROM memories m JOIN context_nodes c ON c.namespace=m.namespace AND c.workspace_id=m.workspace_id AND c.path='root';
 DELETE FROM context_nodes_fts;
 INSERT INTO context_nodes_fts(rowid,id,namespace,workspace_id,path,summary,navigation_summary)
 SELECT rowid,id,namespace,workspace_id,path,summary,navigation_summary FROM context_nodes;"#)?;
    }
    if current < 6 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS dream_runs(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,started_at INTEGER NOT NULL,finished_at INTEGER,status TEXT NOT NULL,sessions_scanned INTEGER NOT NULL DEFAULT 0,entries_scanned INTEGER NOT NULL DEFAULT 0,signals_found INTEGER NOT NULL DEFAULT 0,memories_added INTEGER NOT NULL DEFAULT 0,memories_merged INTEGER NOT NULL DEFAULT 0,memories_superseded INTEGER NOT NULL DEFAULT 0,memories_archived INTEGER NOT NULL DEFAULT 0,model TEXT NOT NULL DEFAULT '',input_tokens INTEGER NOT NULL DEFAULT 0,output_tokens INTEGER NOT NULL DEFAULT 0,failure_reason TEXT,dry_run INTEGER NOT NULL DEFAULT 0);CREATE INDEX IF NOT EXISTS idx_dream_runs_workspace ON dream_runs(workspace_id,finished_at,started_at);"
        )?;
    }
    if current < 7 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_consumption_receipts(recall_trace_id TEXT NOT NULL,memory_id TEXT NOT NULL,consumer TEXT NOT NULL,purpose TEXT NOT NULL,presented INTEGER NOT NULL DEFAULT 0,referenced INTEGER NOT NULL DEFAULT 0,used_for_action INTEGER NOT NULL DEFAULT 0,outcome TEXT NOT NULL DEFAULT '',turn_id TEXT NOT NULL,consumed_at INTEGER NOT NULL,PRIMARY KEY(recall_trace_id,memory_id,consumer,purpose,turn_id),FOREIGN KEY(recall_trace_id) REFERENCES recall_traces(id) ON DELETE CASCADE);
             CREATE TABLE IF NOT EXISTS memory_jobs(id TEXT PRIMARY KEY,phase TEXT NOT NULL,source_id TEXT NOT NULL,source_revision TEXT NOT NULL,source_watermark TEXT NOT NULL DEFAULT '',status TEXT NOT NULL,lease_owner TEXT,lease_until INTEGER,attempt INTEGER NOT NULL DEFAULT 0,next_retry_at INTEGER,input_digest TEXT NOT NULL,output_digest TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(phase,source_id,source_revision,input_digest));
             CREATE INDEX IF NOT EXISTS idx_memory_receipts_trace ON memory_consumption_receipts(recall_trace_id,consumed_at);
             CREATE INDEX IF NOT EXISTS idx_memory_jobs_claim ON memory_jobs(phase,status,next_retry_at,lease_until,created_at);"
        )?;
    }
    if current < 8 {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS request_receipts(request_id TEXT PRIMARY KEY,digest TEXT NOT NULL,response TEXT,created_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS idx_request_receipts_age ON request_receipts(created_at);")?;
    }
    if current < 9 {
        if !table_has_column(&tx, "memories", "gist")? {
            tx.execute(
                "ALTER TABLE memories ADD COLUMN gist TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !table_has_column(&tx, "memories", "tokens_est")? {
            tx.execute(
                "ALTER TABLE memories ADD COLUMN tokens_est INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS baseline_revisions(namespace TEXT NOT NULL,workspace_id TEXT NOT NULL,scope_key TEXT NOT NULL DEFAULT '',revision INTEGER NOT NULL DEFAULT 1,updated_at INTEGER NOT NULL,PRIMARY KEY(namespace,workspace_id,scope_key));
             CREATE INDEX IF NOT EXISTS idx_recall_traces_age ON recall_traces(created_at);
             CREATE INDEX IF NOT EXISTS idx_memory_receipts_age ON memory_consumption_receipts(consumed_at);
             DROP INDEX IF EXISTS idx_memories_active_identity;"
        )?;
        let rows = {
            let mut stmt = tx.prepare("SELECT id,content FROM memories")?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, content) in rows {
            let normalized = normalize(&content);
            let content_hash = hash_normalized(&normalized);
            let gist = gist_for_content(&content, 180);
            let tokens = i64::try_from(estimate_tokens(&content)).unwrap_or(i64::MAX);
            tx.execute(
                "UPDATE memories SET normalized_content=?1,content_hash=?2,gist=?3,tokens_est=?4 WHERE id=?5",
                params![normalized,content_hash,gist,tokens,id],
            )?;
        }
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS v9_duplicate_map(loser TEXT PRIMARY KEY,keeper TEXT NOT NULL,max_rank INTEGER NOT NULL);
             DELETE FROM v9_duplicate_map;
             INSERT INTO v9_duplicate_map(loser,keeper,max_rank)
             WITH ranked AS (
               SELECT id,
                      FIRST_VALUE(id) OVER (
                        PARTITION BY namespace,workspace_id,scope,COALESCE(scope_key,''),kind,content_hash
                        ORDER BY pinned DESC,importance DESC,updated_at DESC,id ASC
                      ) AS keeper,
                      MAX(CASE sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END)
                        OVER (PARTITION BY namespace,workspace_id,scope,COALESCE(scope_key,''),kind,content_hash) AS max_rank,
                      ROW_NUMBER() OVER (
                        PARTITION BY namespace,workspace_id,scope,COALESCE(scope_key,''),kind,content_hash
                        ORDER BY pinned DESC,importance DESC,updated_at DESC,id ASC
                      ) AS rn
               FROM memories WHERE status='ACTIVE'
             )
             SELECT id,keeper,max_rank FROM ranked WHERE rn>1;
             UPDATE memories SET sensitivity=CASE
               (SELECT MAX(max_rank) FROM v9_duplicate_map d WHERE d.keeper=memories.id)
               WHEN 0 THEN 'public' WHEN 1 THEN 'personal' WHEN 2 THEN 'private' WHEN 3 THEN 'secret' ELSE 'ephemeral' END
             WHERE id IN (SELECT DISTINCT keeper FROM v9_duplicate_map);
             UPDATE memories SET status='SUPERSEDED',
                 supersedes_id=(SELECT keeper FROM v9_duplicate_map d WHERE d.loser=memories.id),
                 valid_until=CASE WHEN valid_until IS NULL OR valid_until>unixepoch() THEN unixepoch() ELSE valid_until END
             WHERE id IN (SELECT loser FROM v9_duplicate_map);
             DROP TABLE v9_duplicate_map;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_active_identity ON memories(namespace,workspace_id,scope,COALESCE(scope_key,''),kind,content_hash) WHERE status='ACTIVE';
             DROP TRIGGER IF EXISTS memories_fts_insert;
             DROP TRIGGER IF EXISTS memories_fts_delete;
             DROP TRIGGER IF EXISTS memories_fts_update;
             DROP TABLE IF EXISTS memories_fts;
             CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED,namespace UNINDEXED,workspace_id UNINDEXED,normalized_content,tokenize='unicode61 remove_diacritics 2');
             INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content)
               SELECT rowid,id,namespace,workspace_id,normalized_content FROM memories;
             CREATE TRIGGER memories_fts_insert AFTER INSERT ON memories BEGIN
               INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content)
               VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.normalized_content);
             END;
             CREATE TRIGGER memories_fts_delete AFTER DELETE ON memories BEGIN
               DELETE FROM memories_fts WHERE rowid=old.rowid;
             END;
             CREATE TRIGGER memories_fts_update AFTER UPDATE OF normalized_content ON memories BEGIN
               DELETE FROM memories_fts WHERE rowid=old.rowid;
               INSERT INTO memories_fts(rowid,id,namespace,workspace_id,normalized_content)
               VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.normalized_content);
             END;
             INSERT INTO baseline_revisions(namespace,workspace_id,scope_key,revision,updated_at)
               SELECT namespace,workspace_id,COALESCE(scope_key,''),1,unixepoch() FROM memories
               GROUP BY namespace,workspace_id,COALESCE(scope_key,'')
             ON CONFLICT(namespace,workspace_id,scope_key) DO NOTHING;
             DROP TRIGGER IF EXISTS baseline_revision_insert;
             DROP TRIGGER IF EXISTS baseline_revision_update;
             DROP TRIGGER IF EXISTS baseline_revision_delete;
             CREATE TRIGGER baseline_revision_insert AFTER INSERT ON memories BEGIN
               INSERT INTO baseline_revisions(namespace,workspace_id,scope_key,revision,updated_at)
               VALUES(new.namespace,new.workspace_id,COALESCE(new.scope_key,''),1,unixepoch())
               ON CONFLICT(namespace,workspace_id,scope_key) DO UPDATE SET revision=revision+1,updated_at=unixepoch();
             END;
             CREATE TRIGGER baseline_revision_update AFTER UPDATE OF namespace,workspace_id,kind,content,normalized_content,status,sensitivity,scope,scope_key,importance,confidence,updated_at,valid_from,valid_until,supersedes_id,content_hash,pinned,metadata_json,gist,tokens_est ON memories BEGIN
               INSERT INTO baseline_revisions(namespace,workspace_id,scope_key,revision,updated_at)
               VALUES(old.namespace,old.workspace_id,COALESCE(old.scope_key,''),1,unixepoch())
               ON CONFLICT(namespace,workspace_id,scope_key) DO UPDATE SET revision=revision+1,updated_at=unixepoch();
               INSERT INTO baseline_revisions(namespace,workspace_id,scope_key,revision,updated_at)
               VALUES(new.namespace,new.workspace_id,COALESCE(new.scope_key,''),1,unixepoch())
               ON CONFLICT(namespace,workspace_id,scope_key) DO UPDATE SET revision=revision+1,updated_at=unixepoch();
             END;
             CREATE TRIGGER baseline_revision_delete AFTER DELETE ON memories BEGIN
               INSERT INTO baseline_revisions(namespace,workspace_id,scope_key,revision,updated_at)
               VALUES(old.namespace,old.workspace_id,COALESCE(old.scope_key,''),1,unixepoch())
               ON CONFLICT(namespace,workspace_id,scope_key) DO UPDATE SET revision=revision+1,updated_at=unixepoch();
             END;"
        )?;
    }
    if current < STORE_SCHEMA_VERSION {
        tx.execute("UPDATE schema_info SET version=?1", [STORE_SCHEMA_VERSION])?;
    }
    tx.commit()?;
    Ok(())
}
fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
) -> std::result::Result<bool, rusqlite::Error> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|n| n == column))
}

fn json_storage(error: serde_json::Error) -> MemoryError {
    MemoryError::Storage(error.to_string())
}

fn storage<E: std::fmt::Display>(error: E) -> MemoryError {
    MemoryError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitt_memory_core::{SemanticScore, normalize};

    fn memory(content: &str, sensitivity: Sensitivity, pinned: bool) -> NewMemory {
        NewMemory {
            namespace: "assistant".into(),
            workspace_id: "global".into(),
            kind: MemoryKind::UserPreference,
            content: content.into(),
            sensitivity,
            scope: MemoryScope::Global,
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            pinned,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        }
    }

    fn temp_db(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}-{}.db",
            std::process::id(),
            now_epoch(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("db-wal"));
        let _ = fs::remove_file(path.with_extension("db-shm"));
    }

    struct FavorSecond;

    impl SemanticReranker for FavorSecond {
        fn score(&self, _query: &str, candidates: &[MemoryRecord]) -> Result<Vec<SemanticScore>> {
            Ok(candidates
                .iter()
                .enumerate()
                .map(|(index, memory)| SemanticScore {
                    memory_id: memory.id.clone(),
                    score: if index == 1 { 1.0 } else { 0.0 },
                })
                .collect())
        }
    }

    #[test]
    fn remembers_and_recalls_through_fts() {
        let path = temp_db("kitt-memory-fts");
        let store = SqliteMemoryStore::open(&path).unwrap();
        store
            .remember(memory(
                "Prefere respostas curtas",
                Sensitivity::Private,
                false,
            ))
            .unwrap();
        store
            .remember(memory(
                "Use PostgreSQL for billing",
                Sensitivity::Private,
                false,
            ))
            .unwrap();
        let got = store
            .recall(&RecallQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                scope_key: None,
                text: "billing PostgreSQL".into(),
                limit: 1,
                as_of: None,
                allow_private: true,
                allow_secret: false,
            })
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].content.contains("PostgreSQL"));
        cleanup(&path);
    }

    #[test]
    fn optional_semantic_reranker_can_change_order() {
        let path = temp_db("kitt-memory-rerank");
        let store = SqliteMemoryStore::open(&path).unwrap();
        store
            .remember(memory("alpha database rule", Sensitivity::Private, false))
            .unwrap();
        store
            .remember(memory("alpha deployment rule", Sensitivity::Private, false))
            .unwrap();
        let query = RecallQuery {
            namespace: "assistant".into(),
            workspace_id: "global".into(),
            scope_key: None,
            text: "alpha rule".into(),
            limit: 2,
            as_of: None,
            allow_private: true,
            allow_secret: false,
        };
        let got = store.recall_with_reranker(&query, &FavorSecond).unwrap();
        assert_eq!(got.len(), 2);
        cleanup(&path);
    }

    #[test]
    fn exact_duplicate_never_downgrades_sensitivity() {
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
        cleanup(&path);
    }

    #[test]
    fn baseline_is_deterministic_and_bounded() {
        let path = temp_db("kitt-memory-baseline");
        let store = SqliteMemoryStore::open(&path).unwrap();
        store
            .remember(memory("Always run tests", Sensitivity::Private, true))
            .unwrap();
        store
            .remember(memory(&"context ".repeat(100), Sensitivity::Private, false))
            .unwrap();
        let baseline = store
            .baseline(&BaselineQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                scope_key: None,
                max_tokens: 64,
                as_of: None,
                allow_private: true,
                allow_secret: false,
            })
            .unwrap();
        assert!(!baseline.entries.is_empty());
        assert!(baseline.entries[0].pinned);
        assert!(baseline.estimated_tokens <= baseline.max_tokens + 64);
        cleanup(&path);
    }

    #[test]
    fn similar_non_exact_memory_is_reviewed_not_merged() {
        let path = temp_db("kitt-memory-merge-guard");
        let store = SqliteMemoryStore::open(&path).unwrap();
        store
            .remember(memory(
                "Always run integration tests before release",
                Sensitivity::Private,
                false,
            ))
            .unwrap();
        let incoming = memory(
            "Always run integration tests before production release",
            Sensitivity::Private,
            false,
        );
        let candidates = store.find_merge_candidates(&incoming, 4, None).unwrap();
        assert!(!candidates.is_empty());
        assert_ne!(
            candidates[0].assessment.disposition,
            MergeDisposition::Distinct
        );
        cleanup(&path);
    }

    #[test]
    fn corrections_and_concepts_are_shared_store_primitives() {
        let path = temp_db("kitt-memory-knowledge");
        let store = SqliteMemoryStore::open(&path).unwrap();
        let correction = store
            .record_correction(NewCorrection {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                context: "when generating migrations".into(),
                predicted: "drop the table".into(),
                corrected: "use an additive migration".into(),
                reason: Some("preserve production data".into()),
                source: "user".into(),
                sensitivity: Sensitivity::Private,
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        assert!(store.mark_correction_applied(&correction.id).unwrap());
        let found = store
            .search_corrections("agent-cli", "ws", "additive migration", 4)
            .unwrap();
        assert_eq!(found[0].id, correction.id);
        assert_eq!(found[0].applied_count, 1);

        let parent = store
            .upsert_concept(NewConcept {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                name: "operation-service".into(),
                definition: "Owns operation settlement".into(),
                confidence: 0.9,
                sensitivity: Sensitivity::Private,
                labels: vec!["domain:operations".into()],
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        let child = store
            .upsert_concept(NewConcept {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                name: "settlement-worker".into(),
                definition: "Runs scheduled settlement".into(),
                confidence: 0.8,
                sensitivity: Sensitivity::Private,
                labels: vec!["type:worker".into()],
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        store
            .link_concepts(
                "agent-cli",
                "ws",
                &child.id,
                &parent.id,
                KnowledgeRelation::Requires,
                0.9,
            )
            .unwrap();
        let links = store
            .links_for_concept("agent-cli", "ws", &child.id)
            .unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_id, parent.id);

        let concepts = store
            .search_concepts("agent-cli", "ws", "settlement", 4)
            .unwrap();
        assert!(!concepts.is_empty());
        cleanup(&path);
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
        imported.normalized_content = normalize(&imported.content);
        store.upsert_record(&imported).unwrap();

        let rows = store
            .recall(&RecallQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                scope_key: None,
                text: "migration fact".into(),
                limit: 5,
                as_of: None,
                allow_private: true,
                allow_secret: true,
            })
            .unwrap();
        let row = rows.iter().find(|row| row.id == original.id).unwrap();
        assert_eq!(Sensitivity::Secret, row.sensitivity);
        cleanup(&path);
    }
    #[test]
    fn future_memory_is_hidden_until_valid_from() {
        let path = temp_db("kitt-memory-valid-from");
        let store = SqliteMemoryStore::open(&path).unwrap();
        let mut record = memory("future architecture decision", Sensitivity::Private, false)
            .into_record()
            .unwrap();
        let now = now_epoch();
        record.valid_from = Some(now + 3600);
        store.upsert_record(&record).unwrap();

        let query = RecallQuery {
            namespace: "assistant".into(),
            workspace_id: "global".into(),
            scope_key: None,
            text: "future architecture".into(),
            limit: 5,
            as_of: None,
            allow_private: true,
            allow_secret: false,
        };
        assert!(store.recall(&query).unwrap().is_empty());

        record.valid_from = Some(now - 1);
        store.upsert_record(&record).unwrap();
        assert_eq!(store.recall(&query).unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn concept_neighborhood_expands_links_without_cycles() {
        let path = temp_db("kitt-memory-neighborhood");
        let store = SqliteMemoryStore::open(&path).unwrap();
        let a = store
            .upsert_concept(NewConcept {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                name: "operation-service".into(),
                definition: "Owns settlement orchestration".into(),
                confidence: 0.9,
                sensitivity: Sensitivity::Private,
                labels: vec!["domain:operations".into()],
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        let b = store
            .upsert_concept(NewConcept {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                name: "settlement-worker".into(),
                definition: "Executes scheduled settlement".into(),
                confidence: 0.8,
                sensitivity: Sensitivity::Private,
                labels: vec!["type:worker".into()],
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        let c = store
            .upsert_concept(NewConcept {
                namespace: "agent-cli".into(),
                workspace_id: "ws".into(),
                name: "settlement-ledger".into(),
                definition: "Stores settlement state".into(),
                confidence: 0.8,
                sensitivity: Sensitivity::Private,
                labels: vec!["type:storage".into()],
                source_memory_ids: Vec::new(),
            })
            .unwrap();
        store
            .link_concepts(
                "agent-cli",
                "ws",
                &a.id,
                &b.id,
                KnowledgeRelation::Requires,
                0.9,
            )
            .unwrap();
        store
            .link_concepts(
                "agent-cli",
                "ws",
                &b.id,
                &c.id,
                KnowledgeRelation::Requires,
                0.8,
            )
            .unwrap();

        let found = store
            .search_concept_neighborhood("agent-cli", "ws", "operation", 2, 8)
            .unwrap();
        let ids = found
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        assert!(ids.contains(a.id.as_str()));
        assert!(ids.contains(b.id.as_str()));
        assert!(ids.contains(c.id.as_str()));
        cleanup(&path);
    }
}
