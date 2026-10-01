use super::*;

fn map_context(row: &rusqlite::Row<'_>) -> std::result::Result<ContextNode, rusqlite::Error> {
    let generation = row.get::<_, i64>(7)?;
    let dirty = row.get::<_, i64>(9)?;
    let total = row.get::<_, i64>(10)?;
    if generation < 0 || dirty < 0 || total < 0 {
        return Err(data_error(
            7,
            MemoryError::Corrupt("negative context counters".into()),
        ));
    }
    Ok(ContextNode {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        namespace: row.get(2)?,
        workspace_id: row.get(3)?,
        path: row.get(4)?,
        summary: row.get(5)?,
        navigation_summary: row.get(6)?,
        generation: generation as u64,
        content_hash: row.get(8)?,
        dirty_children: dirty as u64,
        total_children: total as u64,
        summarized_at: row.get(11)?,
        sensitivity: parse_sensitivity_at(row.get::<_, String>(12)?, 12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

const CONTEXT_COLUMNS: &str = "id,parent_id,namespace,workspace_id,path,summary,navigation_summary,generation,content_hash,dirty_children,total_children,summarized_at,sensitivity,created_at,updated_at";

impl SqliteMemoryStore {
    /// Fetch selected memories without broadening conversation scope.
    ///
    /// This is the hydration half of progressive retrieval. Search can return
    /// bounded snippets first; callers explicitly select IDs to hydrate later.
    pub fn get_many_scoped(
        &self,
        namespace: &str,
        workspace_id: &str,
        scope_key: Option<&str>,
        ids: &[String],
        allow_private: bool,
        allow_secret: bool,
    ) -> Result<Vec<MemoryRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let bounded_ids = ids.iter().take(128).cloned().collect::<Vec<_>>();
        let placeholders = (0..bounded_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {MEMORY_COLUMNS} FROM memories m
             WHERE m.namespace=?
               AND (
                 m.scope='global'
                 OR (m.workspace_id=? AND m.scope='workspace')
                 OR (m.workspace_id=? AND m.scope='conversation' AND m.scope_key=?)
               )
               AND m.status='ACTIVE'
               AND m.sensitivity<>'ephemeral'
               AND (m.sensitivity<>'private' OR ?=1)
               AND (m.sensitivity<>'secret' OR ?=1)
               AND m.id IN ({placeholders})"
        );
        let mut values = Vec::<rusqlite::types::Value>::with_capacity(6 + bounded_ids.len());
        values.push(namespace.to_string().into());
        values.push(workspace_id.to_string().into());
        values.push(workspace_id.to_string().into());
        values.push(scope_key.unwrap_or("").to_string().into());
        values.push((allow_private as i64).into());
        values.push((allow_secret as i64).into());
        values.extend(bounded_ids.iter().cloned().map(rusqlite::types::Value::from));
        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(rusqlite::params_from_iter(values), map_memory_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        let by_id = rows
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect::<HashMap<_, _>>();
        Ok(bounded_ids
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect())
    }

    /// Return memories in temporal order, optionally narrowed to one provenance source.
    pub fn timeline_memories(
        &self,
        namespace: &str,
        workspace_id: &str,
        source_id: Option<&str>,
        scope_key: Option<&str>,
        around: Option<i64>,
        limit: usize,
        allow_private: bool,
        allow_secret: bool,
    ) -> Result<Vec<MemoryRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let source = source_id.unwrap_or("");
        let order = if around.is_some() {
            "ABS(m.updated_at-?) ASC,m.updated_at DESC,m.id ASC"
        } else {
            "m.updated_at DESC,m.id ASC"
        };
        let source_clause = if source.is_empty() {
            ""
        } else {
            "AND m.id IN (SELECT memory_id FROM memory_sources WHERE source_id=?)"
        };
        let sql = format!(
            "SELECT {MEMORY_COLUMNS} FROM memories m
             WHERE m.namespace=?
               AND (
                 m.scope='global'
                 OR (m.workspace_id=? AND m.scope='workspace')
                 OR (m.workspace_id=? AND m.scope='conversation' AND m.scope_key=?)
               )
               AND m.status='ACTIVE'
               AND m.sensitivity<>'ephemeral'
               AND (m.sensitivity<>'private' OR ?=1)
               AND (m.sensitivity<>'secret' OR ?=1)
               {source_clause}
             ORDER BY {order}
             LIMIT ?"
        );
        let mut values = Vec::<rusqlite::types::Value>::new();
        values.push(namespace.to_string().into());
        values.push(workspace_id.to_string().into());
        values.push(workspace_id.to_string().into());
        values.push(scope_key.unwrap_or("").to_string().into());
        values.push((allow_private as i64).into());
        values.push((allow_secret as i64).into());
        if !source.is_empty() {
            values.push(source.to_string().into());
        }
        if let Some(at) = around {
            values.push(at.into());
        }
        values.push((limit.min(128) as i64).into());
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(rusqlite::params_from_iter(values), map_memory_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        })
    }
}

impl SemanticMemoryStore for SqliteMemoryStore {
    fn upsert_context_node(&self, node: NewContextNode) -> Result<ContextNode> {
        let node = node.into_record()?;
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        tx.execute(
            "INSERT INTO context_nodes(id,parent_id,namespace,workspace_id,path,summary,navigation_summary,generation,content_hash,dirty_children,total_children,summarized_at,sensitivity,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(namespace,workspace_id,path) DO UPDATE SET
               parent_id=excluded.parent_id,summary=excluded.summary,navigation_summary=excluded.navigation_summary,
               generation=context_nodes.generation+1,content_hash=excluded.content_hash,dirty_children=0,
               total_children=excluded.total_children,summarized_at=excluded.summarized_at,
               sensitivity=CASE WHEN (CASE context_nodes.sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END) >=
                 (CASE excluded.sensitivity WHEN 'public' THEN 0 WHEN 'personal' THEN 1 WHEN 'private' THEN 2 WHEN 'secret' THEN 3 ELSE 4 END)
                 THEN context_nodes.sensitivity ELSE excluded.sensitivity END,
               updated_at=excluded.updated_at",
            params![node.id,node.parent_id,node.namespace,node.workspace_id,node.path,node.summary,node.navigation_summary,
                node.generation as i64,node.content_hash,node.dirty_children as i64,node.total_children as i64,
                node.summarized_at,node.sensitivity.as_db(),node.created_at,node.updated_at],
        ).map_err(storage)?;
        let id: String = tx
            .query_row(
                "SELECT id FROM context_nodes WHERE namespace=?1 AND workspace_id=?2 AND path=?3",
                params![node.namespace, node.workspace_id, node.path],
                |row| row.get(0),
            )
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        self.context_node(&id)?
            .ok_or_else(|| MemoryError::Storage("context node vanished".into()))
    }

    fn context_node(&self, id: &str) -> Result<Option<ContextNode>> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {CONTEXT_COLUMNS} FROM context_nodes WHERE id=?1"),
                [id],
                map_context,
            )
            .optional()
        })
    }

    fn search_context_nodes(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ContextNode>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let q = fts_query(query);
        self.with_conn(|conn| {
            if q.is_empty() {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {CONTEXT_COLUMNS} FROM context_nodes WHERE namespace=?1 AND workspace_id=?2 ORDER BY dirty_children DESC,generation DESC,path ASC LIMIT ?3"
                ))?;
                return stmt.query_map(params![namespace,workspace_id,limit.min(128) as i64], map_context)?
                    .collect::<std::result::Result<Vec<_>,_>>();
            }
            let mut stmt = conn.prepare(&format!(
                "SELECT {CONTEXT_COLUMNS} FROM context_nodes WHERE id IN (
                   SELECT id FROM context_nodes_fts WHERE context_nodes_fts MATCH ?1 AND namespace=?2 AND workspace_id=?3 LIMIT ?4
                 ) ORDER BY dirty_children DESC,generation DESC,path ASC"
            ))?;
            stmt.query_map(params![q,namespace,workspace_id,limit.min(128) as i64], map_context)?
                .collect::<std::result::Result<Vec<_>,_>>()
        })
    }

    fn mark_context_dirty(&self, id: &str, child_delta: u64) -> Result<Option<ContextNode>> {
        let delta = i64::try_from(child_delta).unwrap_or(i64::MAX);
        let conn = self.writer_conn()?;
        conn.execute(
            "UPDATE context_nodes SET dirty_children=MIN(9223372036854775807,dirty_children+?1),updated_at=?2 WHERE id=?3",
            params![delta,now_epoch(),id],
        ).map_err(storage)?;
        drop(conn);
        self.context_node(id)
    }

    fn link_memory_context(&self, memory_id: &str, context_node_id: &str) -> Result<()> {
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO memory_context_links(memory_id,context_node_id,created_at) VALUES(?1,?2,?3)",
            params![memory_id,context_node_id,now_epoch()],
        ).map_err(storage)?;
        Ok(())
    }

    fn record_source(&self, source: NewMemorySource) -> Result<MemorySource> {
        let source = source.into_record()?;
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT INTO memory_sources(id,memory_id,source_kind,source_id,source_uri,source_digest,relationship,source_revision,observed_at,valid_from,valid_until)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT DO UPDATE SET source_uri=excluded.source_uri,source_digest=excluded.source_digest,observed_at=excluded.observed_at,valid_from=excluded.valid_from,valid_until=excluded.valid_until",
            params![source.id,source.memory_id,source.source_kind,source.source_id,source.source_uri,source.source_digest,
                source.relationship,source.source_revision,source.observed_at,source.valid_from,source.valid_until],
        ).map_err(storage)?;
        let stored=conn.query_row(
            "SELECT id,memory_id,source_kind,source_id,source_uri,source_digest,relationship,source_revision,observed_at,valid_from,valid_until
             FROM memory_sources WHERE memory_id=?1 AND source_kind=?2 AND source_id=?3 AND relationship=?4 AND COALESCE(source_revision,'')=COALESCE(?5,'')",
            params![source.memory_id,source.source_kind,source.source_id,source.relationship,source.source_revision],
            |row| Ok(MemorySource{id:row.get(0)?,memory_id:row.get(1)?,source_kind:row.get(2)?,source_id:row.get(3)?,
                source_uri:row.get(4)?,source_digest:row.get(5)?,relationship:row.get(6)?,source_revision:row.get(7)?,
                observed_at:row.get(8)?,valid_from:row.get(9)?,valid_until:row.get(10)?}),
        ).map_err(storage)?;
        Ok(stored)
    }

    fn sources_for_memory(&self, memory_id: &str) -> Result<Vec<MemorySource>> {
        self.with_conn(|conn| {
            let mut stmt=conn.prepare(
                "SELECT id,memory_id,source_kind,source_id,source_uri,source_digest,relationship,source_revision,observed_at,valid_from,valid_until
                 FROM memory_sources WHERE memory_id=?1 ORDER BY observed_at DESC,id ASC"
            )?;
            stmt.query_map([memory_id], |row| Ok(MemorySource{id:row.get(0)?,memory_id:row.get(1)?,source_kind:row.get(2)?,
                source_id:row.get(3)?,source_uri:row.get(4)?,source_digest:row.get(5)?,relationship:row.get(6)?,
                source_revision:row.get(7)?,observed_at:row.get(8)?,valid_from:row.get(9)?,valid_until:row.get(10)?}))?
                .collect::<std::result::Result<Vec<_>,_>>()
        })
    }

    fn record_change_set(&self, set: &MemoryChangeSet, changes: &[MemoryChange]) -> Result<()> {
        let mut conn = self.writer_conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        tx.execute(
            "INSERT OR REPLACE INTO memory_change_sets(id,workspace_id,origin_type,origin_id,started_at,committed_at,model,reason,status)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![set.id,set.workspace_id,set.origin_type,set.origin_id,set.started_at,set.committed_at,set.model,set.reason,set.status],
        ).map_err(storage)?;
        for change in changes {
            tx.execute(
                "INSERT OR REPLACE INTO memory_changes(id,change_set_id,memory_id,operation,before_json,after_json,evidence_json,reason_code)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![change.id,change.change_set_id,change.memory_id,change.operation,change.before_json,change.after_json,change.evidence_json,change.reason_code],
            ).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(())
    }

    fn change_history(&self, workspace_id: &str, limit: usize) -> Result<Vec<MemoryChangeSet>> {
        self.with_conn(|conn| {
            let mut stmt=conn.prepare(
                "SELECT id,workspace_id,origin_type,origin_id,started_at,committed_at,model,reason,status
                 FROM memory_change_sets WHERE workspace_id=?1 ORDER BY started_at DESC,id DESC LIMIT ?2"
            )?;
            stmt.query_map(params![workspace_id,limit.min(256) as i64], |row| Ok(MemoryChangeSet{
                id:row.get(0)?,workspace_id:row.get(1)?,origin_type:row.get(2)?,origin_id:row.get(3)?,
                started_at:row.get(4)?,committed_at:row.get(5)?,model:row.get(6)?,reason:row.get(7)?,status:row.get(8)?
            }))?.collect::<std::result::Result<Vec<_>,_>>()
        })
    }

    fn record_recall_trace(&self, trace: &RecallTrace) -> Result<()> {
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO recall_traces(id,namespace,workspace_id,query,planned_scopes_json,candidates_json,selected_json,token_cost,semantic_fallback,elapsed_us,context_hash,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![trace.id,trace.namespace,trace.workspace_id,trace.query,trace.planned_scopes_json,trace.candidates_json,
                trace.selected_json,trace.token_cost as i64,trace.semantic_fallback as i64,trace.elapsed_us as i64,trace.context_hash,trace.created_at],
        ).map_err(storage)?;
        Ok(())
    }

    fn recent_recall_traces(&self, workspace_id: &str, limit: usize) -> Result<Vec<RecallTrace>> {
        self.with_conn(|conn| {
            let mut stmt=conn.prepare(
                "SELECT id,namespace,workspace_id,query,planned_scopes_json,candidates_json,selected_json,token_cost,semantic_fallback,elapsed_us,context_hash,created_at
                 FROM recall_traces WHERE workspace_id=?1 ORDER BY created_at DESC,id DESC LIMIT ?2"
            )?;
            stmt.query_map(params![workspace_id,limit.min(256) as i64], |row| {
                let token:i64=row.get(7)?; let elapsed:i64=row.get(9)?;
                if token<0 || elapsed<0 { return Err(data_error(7,MemoryError::Corrupt("negative recall trace counter".into()))); }
                Ok(RecallTrace{id:row.get(0)?,namespace:row.get(1)?,workspace_id:row.get(2)?,query:row.get(3)?,
                    planned_scopes_json:row.get(4)?,candidates_json:row.get(5)?,selected_json:row.get(6)?,
                    token_cost:token as u64,semantic_fallback:row.get::<_,i64>(8)?!=0,elapsed_us:elapsed as u64,
                    context_hash:row.get(10)?,created_at:row.get(11)?})
            })?.collect::<std::result::Result<Vec<_>,_>>()
        })
    }

    fn register_schema(&self, schema: &MemorySchemaDefinition) -> Result<()> {
        for raw in [
            &schema.fields_schema_json,
            &schema.retention_policy_json,
            &schema.merge_policy_json,
            &schema.index_fields_json,
        ] {
            serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|e| MemoryError::Invalid(format!("memory schema JSON is invalid: {e}")))?;
        }
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO memory_schemas(schema_id,version,base_kind,fields_schema_json,retention_policy_json,merge_policy_json,default_sensitivity,index_fields_json,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![schema.schema_id,schema.version as i64,schema.base_kind,schema.fields_schema_json,schema.retention_policy_json,
                schema.merge_policy_json,schema.default_sensitivity.as_db(),schema.index_fields_json,schema.updated_at],
        ).map_err(storage)?;
        Ok(())
    }

    fn list_schemas(&self) -> Result<Vec<MemorySchemaDefinition>> {
        self.with_conn(|conn| {
            let mut stmt=conn.prepare(
                "SELECT schema_id,version,base_kind,fields_schema_json,retention_policy_json,merge_policy_json,default_sensitivity,index_fields_json,updated_at
                 FROM memory_schemas ORDER BY schema_id ASC,version DESC"
            )?;
            stmt.query_map([], |row| {
                let version:i64=row.get(1)?;
                if version<0 { return Err(data_error(1,MemoryError::Corrupt("negative memory schema version".into()))); }
                Ok(MemorySchemaDefinition{schema_id:row.get(0)?,version:version as u64,base_kind:row.get(2)?,
                    fields_schema_json:row.get(3)?,retention_policy_json:row.get(4)?,merge_policy_json:row.get(5)?,
                    default_sensitivity:parse_sensitivity_at(row.get::<_,String>(6)?,6)?,
                    index_fields_json:row.get(7)?,updated_at:row.get(8)?})
            })?.collect::<std::result::Result<Vec<_>,_>>()
        })
    }
}
