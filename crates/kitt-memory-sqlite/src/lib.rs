use kitt_memory_core::{
    BaselineQuery, CorrectionRecord, KnowledgeEdge, KnowledgeRelation, KnowledgeStore,
    MemoryBaseline, MemoryError, MemoryKind, MemoryRecord, MemoryScope, MemoryStatus,
    MemoryStore, MergeCandidate, MergeDisposition, NewConcept, NewCorrection, NewMemory,
    RecallQuery, Result, SemanticReranker, Sensitivity, StoredConcept,
    assess_merge_candidate, build_memory_baseline, now_epoch,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const STORE_SCHEMA_VERSION: i64 = 2;
const MEMORY_COLUMNS: &str =
    "m.id,m.namespace,m.workspace_id,m.kind,m.content,m.normalized_content,m.status,m.sensitivity,m.scope,m.importance,m.confidence,m.created_at,m.updated_at,m.last_accessed_at,m.access_count,m.valid_until,m.supersedes_id,m.content_hash,m.pinned,m.metadata_json";

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

fn open_legacy_read_only(path: &Path) -> Result<Connection> {
    let metadata = fs::symlink_metadata(path).map_err(storage)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MemoryError::Storage(format!(
            "legacy memory source must be a regular non-symlink file: {}",
            path.display()
        )));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(storage)
}

pub struct SqliteMemoryStore {
    path: PathBuf,
    writer: Mutex<Connection>,
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(storage)?;
        }
        ensure_private_database_file(&path)?;
        let writer = Self::open_connection(&path)?;
        let store = Self {
            path,
            writer: Mutex::new(writer),
        };
        {
            let conn = store.writer_conn()?;
            migrate(&conn).map_err(storage)?;
        }
        store.prune_expired()?;
        Ok(store)
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path).map_err(storage)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(storage)?;
        Ok(conn)
    }

    fn conn(&self) -> Result<Connection> {
        Self::open_connection(&self.path)
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
        f(&conn).map_err(storage)
    }

    pub fn import_legacy_agent_db(&self, source: impl AsRef<Path>) -> Result<usize> {
        let source = open_legacy_read_only(source.as_ref())?;
        let mut stmt = source.prepare(
            "SELECT id, workspace_id, kind, content, normalized_content, status, importance,              confidence, created_at, updated_at, last_accessed_at, access_count, valid_until,              supersedes_id, content_hash, pinned, COALESCE(metadata_json,'{}') FROM memories",
        ).map_err(storage)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MemoryRecord {
                    id: row.get(0)?,
                    namespace: "agent-cli".into(),
                    workspace_id: row.get(1)?,
                    kind: MemoryKind::from_db(&row.get::<_, String>(2)?),
                    content: row.get(3)?,
                    normalized_content: row.get(4)?,
                    status: MemoryStatus::from_db(&row.get::<_, String>(5)?),
                    sensitivity: Sensitivity::Private,
                    scope: MemoryScope::Workspace,
                    importance: row.get::<_, f64>(6)? as f32,
                    confidence: row.get::<_, f64>(7)? as f32,
                    created_at: row.get::<_, f64>(8)? as i64,
                    updated_at: row.get::<_, f64>(9)? as i64,
                    last_accessed_at: row.get::<_, Option<f64>>(10)?.map(|value| value as i64),
                    access_count: row.get::<_, i64>(11)? as u64,
                    valid_until: row.get::<_, Option<f64>>(12)?.map(|value| value as i64),
                    supersedes_id: row.get(13)?,
                    content_hash: row.get(14)?,
                    pinned: row.get::<_, i64>(15)? != 0,
                    metadata_json: row.get(16)?,
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

    fn load_recall_candidates(
        &self,
        query: &RecallQuery,
        cap: usize,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        let now = now_epoch();
        let fts = fts_query(&query.text);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let sql = format!(
                    "SELECT {MEMORY_COLUMNS}, bm25(memories_fts)                      FROM memories_fts JOIN memories m ON m.rowid=memories_fts.rowid                      WHERE memories_fts MATCH ?1 AND m.namespace=?2                        AND (m.workspace_id=?3 OR m.scope='global')                        AND m.status='ACTIVE'                        AND (m.valid_until IS NULL OR m.valid_until>?4)                      ORDER BY bm25(memories_fts) ASC LIMIT ?5"
                );
                let mut stmt = conn.prepare(&sql)?;
                stmt.query_map(
                    params![fts, query.namespace, query.workspace_id, now, cap as i64],
                    |row| Ok((map_memory_row(row)?, row.get::<_, f64>(20)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()
            })?
        };

        if rows.len() < cap {
            let fallback = self.with_conn(|conn| {
                let sql = format!(
                    "SELECT {MEMORY_COLUMNS}, 0.0 FROM memories m WHERE m.namespace=?1 AND (m.workspace_id=?2 OR m.scope='global') AND m.status='ACTIVE' AND (m.valid_until IS NULL OR m.valid_until>?3) ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT ?4"
                );
                let mut stmt = conn.prepare(&sql)?;
                stmt.query_map(
                    params![query.namespace, query.workspace_id, now, cap as i64],
                    |row| Ok((map_memory_row(row)?, row.get::<_, f64>(20)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()
            })?;
            let mut seen = rows
                .iter()
                .map(|(memory, _)| memory.id.clone())
                .collect::<std::collections::HashSet<_>>();
            for candidate in fallback {
                if rows.len() >= cap {
                    break;
                }
                if seen.insert(candidate.0.id.clone()) {
                    rows.push(candidate);
                }
            }
        }

        rows.retain(|(memory, _)| sensitivity_allowed(memory.sensitivity, query));
        Ok(rows)
    }

    fn ranked_recall(
        &self,
        query: &RecallQuery,
        reranker: Option<&dyn SemanticReranker>,
        touch: bool,
    ) -> Result<Vec<MemoryRecord>> {
        let limit = query.limit.clamp(1, 50);
        let cap = (limit.saturating_mul(12)).clamp(64, 256);
        let candidates = self.load_recall_candidates(query, cap)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let candidate_records = candidates
            .iter()
            .map(|(memory, _)| memory.clone())
            .collect::<Vec<_>>();
        let semantic_scores = reranker
            .and_then(|ranker| ranker.score(&query.text, &candidate_records).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|item| (item.memory_id, item.score.clamp(0.0, 1.0)))
            .collect::<HashMap<_, _>>();

        let now = now_epoch();
        let semantic_enabled = !semantic_scores.is_empty();
        let mut ranked = candidates
            .into_iter()
            .enumerate()
            .map(|(index, (memory, _bm25))| {
                let lexical = kitt_memory_core::lexical_similarity(&query.text, &memory);
                let retained = kitt_memory_core::retention_score(&memory, now).min(1.25);
                let retrieval_position = 1.0 / (1.0 + (index as f32 * 0.12));
                let semantic = semantic_scores.get(&memory.id).copied().unwrap_or(0.0);
                let score = if semantic_enabled {
                    lexical * 0.30
                        + semantic * 0.35
                        + retained * 0.20
                        + retrieval_position * 0.15
                } else {
                    lexical * 0.48 + retained * 0.32 + retrieval_position * 0.20
                };
                (score, memory)
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        let selected = ranked
            .into_iter()
            .take(limit)
            .map(|(_, memory)| memory)
            .collect::<Vec<_>>();

        if touch && !selected.is_empty() {
            self.touch_access(selected.iter().map(|memory| memory.id.as_str()))?;
        }
        Ok(selected)
    }

    fn touch_access<'a>(&self, ids: impl Iterator<Item = &'a str>) -> Result<()> {
        let ids = ids.map(str::to_owned).collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = (0..ids.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE memories SET last_accessed_at=?1,access_count=access_count+1              WHERE id IN ({placeholders})"
        );
        let mut values = Vec::<rusqlite::types::Value>::with_capacity(ids.len() + 1);
        values.push(now_epoch().into());
        values.extend(ids.into_iter().map(rusqlite::types::Value::from));
        let conn = self.writer_conn()?;
        conn.execute(&sql, rusqlite::params_from_iter(values))
            .map_err(storage)?;
        Ok(())
    }
}

impl MemoryStore for SqliteMemoryStore {
    fn remember(&self, memory: NewMemory) -> Result<MemoryRecord> {
        if memory.sensitivity == Sensitivity::Ephemeral {
            return memory.into_record();
        }
        let record = memory.into_record()?;
        let conn = self.writer_conn()?;
        let tx = conn.unchecked_transaction().map_err(storage)?;

        let existing_id = tx
            .query_row(
                "SELECT id FROM memories WHERE namespace=?1 AND workspace_id=?2                  AND content_hash=?3 AND status='ACTIVE' LIMIT 1",
                params![record.namespace, record.workspace_id, record.content_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;

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
                "UPDATE memories SET sensitivity=?1,pinned=?2,importance=?3,confidence=?4,                 valid_until=?5,updated_at=?6 WHERE id=?7",
                params![
                    sensitivity.as_db(),
                    pinned as i64,
                    importance,
                    confidence,
                    valid_until,
                    updated_at,
                    existing_id
                ],
            )
            .map_err(storage)?;
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
        let conn = self.writer_conn()?;
        let mut merged = memory.clone();
        if let Some(existing) = load_one(&conn, &memory.id).map_err(storage)? {
            merged.sensitivity = existing.sensitivity.most_restrictive(memory.sensitivity);
        }
        conn.execute(
            "INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,             sensitivity,scope,importance,confidence,created_at,updated_at,last_accessed_at,             access_count,valid_until,supersedes_id,content_hash,pinned,metadata_json)              VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)              ON CONFLICT(id) DO UPDATE SET              namespace=excluded.namespace,workspace_id=excluded.workspace_id,kind=excluded.kind,             content=excluded.content,normalized_content=excluded.normalized_content,             status=excluded.status,sensitivity=excluded.sensitivity,scope=excluded.scope,             importance=excluded.importance,confidence=excluded.confidence,             updated_at=excluded.updated_at,last_accessed_at=excluded.last_accessed_at,             access_count=excluded.access_count,valid_until=excluded.valid_until,             supersedes_id=excluded.supersedes_id,content_hash=excluded.content_hash,             pinned=excluded.pinned,metadata_json=excluded.metadata_json",
            params![
                merged.id,
                merged.namespace,
                merged.workspace_id,
                merged.kind.as_db(),
                merged.content,
                merged.normalized_content,
                merged.status.as_db(),
                merged.sensitivity.as_db(),
                merged.scope.as_db(),
                merged.importance,
                merged.confidence,
                merged.created_at,
                merged.updated_at,
                merged.last_accessed_at,
                merged.access_count as i64,
                merged.valid_until,
                merged.supersedes_id,
                merged.content_hash,
                merged.pinned as i64,
                merged.metadata_json
            ],
        )
        .map_err(storage)?;
        Ok(())
    }

    fn recall(&self, query: &RecallQuery) -> Result<Vec<MemoryRecord>> {
        self.ranked_recall(query, None, true)
    }

    fn recall_with_reranker(
        &self,
        query: &RecallQuery,
        reranker: &dyn SemanticReranker,
    ) -> Result<Vec<MemoryRecord>> {
        self.ranked_recall(query, Some(reranker), true)
    }

    fn baseline(&self, query: &BaselineQuery) -> Result<MemoryBaseline> {
        let now = now_epoch();
        let records = self.with_conn(|conn| {
            let sql = format!(
                "SELECT {MEMORY_COLUMNS} FROM memories m                  WHERE m.namespace=?1 AND (m.workspace_id=?2 OR m.scope='global')                    AND m.status='ACTIVE' AND (m.valid_until IS NULL OR m.valid_until>?3)                  ORDER BY m.pinned DESC,m.importance DESC,m.updated_at DESC LIMIT 512"
            );
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(params![query.namespace, query.workspace_id, now], map_memory_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        Ok(build_memory_baseline(records, query))
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
            text: memory.content.clone(),
            limit: limit.clamp(1, 32),
            allow_private: true,
            allow_secret: true,
        };
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
        Ok(conn
            .execute(
                "UPDATE memories SET status=?1,supersedes_id=?2,updated_at=?3 WHERE id=?4",
                params![status.as_db(), supersedes_id, now_epoch(), id],
            )
            .map_err(storage)?
            > 0)
    }

    fn prune_expired(&self) -> Result<usize> {
        let conn = self.writer_conn()?;
        conn.execute(
            "DELETE FROM memories WHERE valid_until IS NOT NULL AND valid_until<=?1",
            [now_epoch()],
        )
        .map_err(storage)
    }

    fn consolidate_exact_duplicates(&self, namespace: &str, workspace_id: &str) -> Result<usize> {
        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT content_hash,id FROM memories                  WHERE namespace=?1 AND workspace_id=?2 AND status='ACTIVE'                  ORDER BY pinned DESC,importance DESC,updated_at DESC",
            )?;
            stmt.query_map(params![namespace, workspace_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
        })?;

        let mut keeper: HashMap<String, String> = HashMap::new();
        let mut superseded = Vec::new();
        for (hash, id) in rows {
            if let Some(keep) = keeper.get(&hash) {
                superseded.push((id, keep.clone()));
            } else {
                keeper.insert(hash, id);
            }
        }
        if superseded.is_empty() {
            return Ok(0);
        }

        let conn = self.writer_conn()?;
        let tx = conn.unchecked_transaction().map_err(storage)?;
        let updated_at = now_epoch();
        let mut changed = 0;
        for (id, keep) in &superseded {
            changed += tx
                .execute(
                    "UPDATE memories SET status='SUPERSEDED',supersedes_id=?1,updated_at=?2                      WHERE id=?3 AND status='ACTIVE'",
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
            "INSERT INTO corrections(id,namespace,workspace_id,context,predicted,corrected,reason,             source,applied_count,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                record.id,
                record.namespace,
                record.workspace_id,
                record.context,
                record.predicted,
                record.corrected,
                record.reason,
                record.source,
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
        let limit = limit.clamp(1, 50);
        let fts = fts_query(query);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id,c.namespace,c.workspace_id,c.context,c.predicted,c.corrected,                     c.reason,c.source,c.applied_count,c.created_at,c.updated_at                      FROM corrections_fts JOIN corrections c ON c.rowid=corrections_fts.rowid                      WHERE corrections_fts MATCH ?1 AND c.namespace=?2 AND c.workspace_id=?3                      ORDER BY bm25(corrections_fts) ASC,c.applied_count DESC LIMIT ?4",
                )?;
                stmt.query_map(params![fts, namespace, workspace_id, limit as i64], map_correction)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?
        };
        if rows.is_empty() {
            rows = self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id,namespace,workspace_id,context,predicted,corrected,reason,source,                     applied_count,created_at,updated_at FROM corrections                      WHERE namespace=?1 AND workspace_id=?2                      ORDER BY applied_count DESC,updated_at DESC LIMIT ?3",
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
        let conn = self.writer_conn()?;
        let tx = conn.unchecked_transaction().map_err(storage)?;

        let existing = tx
            .query_row(
                "SELECT id,namespace,workspace_id,name,definition,confidence,revision,                 labels_json,source_memory_ids_json,created_at,updated_at                  FROM concepts WHERE namespace=?1 AND workspace_id=?2 AND name=?3",
                params![incoming.namespace, incoming.workspace_id, incoming.name],
                map_concept,
            )
            .optional()
            .map_err(storage)?;

        let stored = if let Some(mut existing) = existing {
            existing.definition = incoming.definition;
            existing.confidence = existing.confidence.max(incoming.confidence);
            existing.revision = existing.revision.saturating_add(1);
            existing.labels = merged_strings(existing.labels, incoming.labels);
            existing.source_memory_ids =
                merged_strings(existing.source_memory_ids, incoming.source_memory_ids);
            existing.updated_at = now_epoch();
            tx.execute(
                "UPDATE concepts SET definition=?1,confidence=?2,revision=?3,labels_json=?4,                 source_memory_ids_json=?5,updated_at=?6 WHERE id=?7",
                params![
                    existing.definition,
                    existing.confidence,
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
                "INSERT INTO concepts(id,namespace,workspace_id,name,definition,confidence,revision,                 labels_json,source_memory_ids_json,created_at,updated_at)                  VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    incoming.id,
                    incoming.namespace,
                    incoming.workspace_id,
                    incoming.name,
                    incoming.definition,
                    incoming.confidence,
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
        let limit = limit.clamp(1, 50);
        let fts = fts_query(query);
        let mut rows = if fts.is_empty() {
            Vec::new()
        } else {
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id,c.namespace,c.workspace_id,c.name,c.definition,c.confidence,                     c.revision,c.labels_json,c.source_memory_ids_json,c.created_at,c.updated_at                      FROM concepts_fts JOIN concepts c ON c.rowid=concepts_fts.rowid                      WHERE concepts_fts MATCH ?1 AND c.namespace=?2 AND c.workspace_id=?3                      ORDER BY bm25(concepts_fts) ASC,c.confidence DESC LIMIT ?4",
                )?;
                stmt.query_map(params![fts, namespace, workspace_id, limit as i64], map_concept)?
                    .collect::<std::result::Result<Vec<_>, _>>()
            })?
        };
        if rows.is_empty() {
            rows = self.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id,namespace,workspace_id,name,definition,confidence,revision,                     labels_json,source_memory_ids_json,created_at,updated_at FROM concepts                      WHERE namespace=?1 AND workspace_id=?2                      ORDER BY confidence DESC,updated_at DESC LIMIT ?3",
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
            return Err(MemoryError::Invalid("a concept cannot link to itself".into()));
        }
        if !weight.is_finite() {
            return Err(MemoryError::Invalid("knowledge link weight must be finite".into()));
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

        let existing_id = conn
            .query_row(
                "SELECT id FROM concept_links WHERE namespace=?1 AND workspace_id=?2                  AND source_id=?3 AND target_id=?4 AND relation=?5",
                params![
                    namespace,
                    workspace_id,
                    source_id,
                    target_id,
                    relation.as_db()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?;

        let id = existing_id.unwrap_or_else(|| format!("edge_{}", uuid::Uuid::new_v4().simple()));
        conn.execute(
            "INSERT INTO concept_links(id,namespace,workspace_id,source_id,target_id,relation,weight,created_at)              VALUES(?1,?2,?3,?4,?5,?6,?7,?8)              ON CONFLICT(namespace,workspace_id,source_id,target_id,relation)              DO UPDATE SET weight=excluded.weight",
            params![
                id,
                namespace,
                workspace_id,
                source_id,
                target_id,
                relation.as_db(),
                weight,
                now
            ],
        )
        .map_err(storage)?;

        Ok(KnowledgeEdge {
            id,
            namespace: namespace.into(),
            workspace_id: workspace_id.into(),
            source_id: source_id.into(),
            target_id: target_id.into(),
            relation,
            weight,
            created_at: now,
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
                        relation: KnowledgeRelation::from_db(&row.get::<_, String>(5)?),
                        weight: row.get::<_, f64>(6)? as f32,
                        created_at: row.get(7)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
        })
    }
}

fn sensitivity_allowed(sensitivity: Sensitivity, query: &RecallQuery) -> bool {
    match sensitivity {
        Sensitivity::Secret => query.allow_secret,
        Sensitivity::Private => query.allow_private,
        Sensitivity::Ephemeral => false,
        _ => true,
    }
}

fn fts_query(input: &str) -> String {
    let mut terms = input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
        .take(16)
        .map(|term| format!(""{}"", term.replace('"', "")))
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

fn merge_valid_until(existing: Option<i64>, incoming: Option<i64>) -> Option<i64> {
    match (existing, incoming) {
        (None, _) | (_, None) => None,
        (Some(left), Some(right)) => Some(left.max(right)),
    }
}

fn insert_record(
    conn: &Connection,
    memory: &MemoryRecord,
) -> std::result::Result<usize, rusqlite::Error> {
    conn.execute(
        "INSERT INTO memories(id,namespace,workspace_id,kind,content,normalized_content,status,         sensitivity,scope,importance,confidence,created_at,updated_at,last_accessed_at,access_count,         valid_until,supersedes_id,content_hash,pinned,metadata_json)          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
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
            memory.importance,
            memory.confidence,
            memory.created_at,
            memory.updated_at,
            memory.last_accessed_at,
            memory.access_count as i64,
            memory.valid_until,
            memory.supersedes_id,
            memory.content_hash,
            memory.pinned as i64,
            memory.metadata_json
        ],
    )
}

fn map_memory_row(row: &rusqlite::Row<'_>) -> std::result::Result<MemoryRecord, rusqlite::Error> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        kind: MemoryKind::from_db(&row.get::<_, String>(3)?),
        content: row.get(4)?,
        normalized_content: row.get(5)?,
        status: MemoryStatus::from_db(&row.get::<_, String>(6)?),
        sensitivity: Sensitivity::from_db(&row.get::<_, String>(7)?),
        scope: MemoryScope::from_db(&row.get::<_, String>(8)?),
        importance: row.get::<_, f64>(9)? as f32,
        confidence: row.get::<_, f64>(10)? as f32,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        last_accessed_at: row.get(13)?,
        access_count: row.get::<_, i64>(14)? as u64,
        valid_until: row.get(15)?,
        supersedes_id: row.get(16)?,
        content_hash: row.get(17)?,
        pinned: row.get::<_, i64>(18)? != 0,
        metadata_json: row.get(19)?,
    })
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
    Ok(CorrectionRecord {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        context: row.get(3)?,
        predicted: row.get(4)?,
        corrected: row.get(5)?,
        reason: row.get(6)?,
        source: row.get(7)?,
        applied_count: row.get::<_, i64>(8)? as u64,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn map_concept(row: &rusqlite::Row<'_>) -> std::result::Result<StoredConcept, rusqlite::Error> {
    let labels_json: String = row.get(7)?;
    let source_json: String = row.get(8)?;
    Ok(StoredConcept {
        id: row.get(0)?,
        namespace: row.get(1)?,
        workspace_id: row.get(2)?,
        name: row.get(3)?,
        definition: row.get(4)?,
        confidence: row.get::<_, f64>(5)? as f32,
        revision: row.get::<_, i64>(6)? as u64,
        labels: serde_json::from_str(&labels_json).unwrap_or_default(),
        source_memory_ids: serde_json::from_str(&source_json).unwrap_or_default(),
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn migrate(conn: &Connection) -> std::result::Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        r#"
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
        CREATE INDEX IF NOT EXISTS idx_memories_lookup
          ON memories(namespace,workspace_id,status,pinned,importance);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_active_hash
          ON memories(namespace,workspace_id,content_hash) WHERE status='ACTIVE';

        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
          id UNINDEXED, namespace UNINDEXED, workspace_id UNINDEXED, content, normalized_content,
          tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS memories_fts_insert AFTER INSERT ON memories BEGIN
          INSERT INTO memories_fts(rowid,id,namespace,workspace_id,content,normalized_content)
          VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.content,new.normalized_content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_fts_delete AFTER DELETE ON memories BEGIN
          DELETE FROM memories_fts WHERE rowid=old.rowid;
        END;
        CREATE TRIGGER IF NOT EXISTS memories_fts_update AFTER UPDATE ON memories BEGIN
          DELETE FROM memories_fts WHERE rowid=old.rowid;
          INSERT INTO memories_fts(rowid,id,namespace,workspace_id,content,normalized_content)
          VALUES(new.rowid,new.id,new.namespace,new.workspace_id,new.content,new.normalized_content);
        END;

        CREATE TABLE IF NOT EXISTS corrections(
          id TEXT PRIMARY KEY, namespace TEXT NOT NULL, workspace_id TEXT NOT NULL,
          context TEXT NOT NULL, predicted TEXT NOT NULL, corrected TEXT NOT NULL,
          reason TEXT, source TEXT NOT NULL, applied_count INTEGER NOT NULL DEFAULT 0,
          created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_corrections_scope
          ON corrections(namespace,workspace_id,applied_count,updated_at);
        CREATE VIRTUAL TABLE IF NOT EXISTS corrections_fts USING fts5(
          id UNINDEXED, context, predicted, corrected, reason, tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS corrections_fts_insert AFTER INSERT ON corrections BEGIN
          INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason)
          VALUES(new.rowid,new.id,new.context,new.predicted,new.corrected,COALESCE(new.reason,''));
        END;
        CREATE TRIGGER IF NOT EXISTS corrections_fts_delete AFTER DELETE ON corrections BEGIN
          DELETE FROM corrections_fts WHERE rowid=old.rowid;
        END;
        CREATE TRIGGER IF NOT EXISTS corrections_fts_update AFTER UPDATE ON corrections BEGIN
          DELETE FROM corrections_fts WHERE rowid=old.rowid;
          INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason)
          VALUES(new.rowid,new.id,new.context,new.predicted,new.corrected,COALESCE(new.reason,''));
        END;

        CREATE TABLE IF NOT EXISTS concepts(
          id TEXT PRIMARY KEY, namespace TEXT NOT NULL, workspace_id TEXT NOT NULL,
          name TEXT NOT NULL, definition TEXT NOT NULL, confidence REAL NOT NULL,
          revision INTEGER NOT NULL DEFAULT 1, labels_json TEXT NOT NULL DEFAULT '[]',
          source_memory_ids_json TEXT NOT NULL DEFAULT '[]',
          created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
          UNIQUE(namespace,workspace_id,name)
        );
        CREATE INDEX IF NOT EXISTS idx_concepts_scope
          ON concepts(namespace,workspace_id,confidence,updated_at);
        CREATE VIRTUAL TABLE IF NOT EXISTS concepts_fts USING fts5(
          id UNINDEXED, name, definition, labels, tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS concepts_fts_insert AFTER INSERT ON concepts BEGIN
          INSERT INTO concepts_fts(rowid,id,name,definition,labels)
          VALUES(new.rowid,new.id,new.name,new.definition,new.labels_json);
        END;
        CREATE TRIGGER IF NOT EXISTS concepts_fts_delete AFTER DELETE ON concepts BEGIN
          DELETE FROM concepts_fts WHERE rowid=old.rowid;
        END;
        CREATE TRIGGER IF NOT EXISTS concepts_fts_update AFTER UPDATE ON concepts BEGIN
          DELETE FROM concepts_fts WHERE rowid=old.rowid;
          INSERT INTO concepts_fts(rowid,id,name,definition,labels)
          VALUES(new.rowid,new.id,new.name,new.definition,new.labels_json);
        END;

        CREATE TABLE IF NOT EXISTS concept_links(
          id TEXT PRIMARY KEY, namespace TEXT NOT NULL, workspace_id TEXT NOT NULL,
          source_id TEXT NOT NULL, target_id TEXT NOT NULL, relation TEXT NOT NULL,
          weight REAL NOT NULL, created_at INTEGER NOT NULL,
          UNIQUE(namespace,workspace_id,source_id,target_id,relation),
          CHECK(source_id<>target_id),
          FOREIGN KEY(source_id) REFERENCES concepts(id) ON DELETE CASCADE,
          FOREIGN KEY(target_id) REFERENCES concepts(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_concept_links_source
          ON concept_links(namespace,workspace_id,source_id);
        CREATE INDEX IF NOT EXISTS idx_concept_links_target
          ON concept_links(namespace,workspace_id,target_id);
        "#,
    )?;

    let current = conn
        .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| row.get::<_, i64>(0))
        .unwrap_or(1);
    if current < STORE_SCHEMA_VERSION {
        conn.execute_batch(
            r#"
            DELETE FROM memories_fts;
            INSERT INTO memories_fts(rowid,id,namespace,workspace_id,content,normalized_content)
              SELECT rowid,id,namespace,workspace_id,content,normalized_content FROM memories;
            DELETE FROM corrections_fts;
            INSERT INTO corrections_fts(rowid,id,context,predicted,corrected,reason)
              SELECT rowid,id,context,predicted,corrected,COALESCE(reason,'') FROM corrections;
            DELETE FROM concepts_fts;
            INSERT INTO concepts_fts(rowid,id,name,definition,labels)
              SELECT rowid,id,name,definition,labels_json FROM concepts;
            "#,
        )?;
        conn.execute(
            "UPDATE schema_info SET version=?1",
            [STORE_SCHEMA_VERSION],
        )?;
    }
    Ok(())
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
            .remember(memory("Prefere respostas curtas", Sensitivity::Private, false))
            .unwrap();
        store
            .remember(memory("Use PostgreSQL for billing", Sensitivity::Private, false))
            .unwrap();
        let got = store
            .recall(&RecallQuery {
                namespace: "assistant".into(),
                workspace_id: "global".into(),
                text: "billing PostgreSQL".into(),
                limit: 1,
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
            text: "alpha rule".into(),
            limit: 2,
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
                max_tokens: 64,
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
                text: "migration fact".into(),
                limit: 5,
                allow_private: true,
                allow_secret: true,
            })
            .unwrap();
        let row = rows.iter().find(|row| row.id == original.id).unwrap();
        assert_eq!(Sensitivity::Secret, row.sensitivity);
        cleanup(&path);
    }
}
