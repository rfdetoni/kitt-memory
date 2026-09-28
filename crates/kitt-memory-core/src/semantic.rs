use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MemoryError, Result, Sensitivity, now_epoch};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub namespace: String,
    pub workspace_id: String,
    pub path: String,
    pub summary: String,
    pub navigation_summary: String,
    pub generation: u64,
    pub content_hash: String,
    pub dirty_children: u64,
    pub total_children: u64,
    pub summarized_at: Option<i64>,
    pub sensitivity: Sensitivity,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ContextNode {
    pub fn dirty_ratio(&self) -> f32 {
        if self.total_children == 0 {
            return if self.dirty_children > 0 { 1.0 } else { 0.0 };
        }
        self.dirty_children as f32 / self.total_children as f32
    }

    pub fn should_refresh(&self, threshold: f32) -> bool {
        self.dirty_children > 0 && self.dirty_ratio() >= threshold.clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone)]
pub struct NewContextNode {
    pub parent_id: Option<String>,
    pub namespace: String,
    pub workspace_id: String,
    pub path: String,
    pub summary: String,
    pub navigation_summary: String,
    pub content_hash: String,
    pub total_children: u64,
    pub sensitivity: Sensitivity,
}

impl NewContextNode {
    pub fn into_record(self) -> Result<ContextNode> {
        let path = self.path.trim().trim_matches('/').to_string();
        if self.namespace.trim().is_empty() || self.workspace_id.trim().is_empty() || path.is_empty() {
            return Err(MemoryError::Invalid("context node identity is incomplete".into()));
        }
        let now = now_epoch();
        Ok(ContextNode {
            id: format!("ctx_{}", Uuid::new_v4().simple()),
            parent_id: self.parent_id,
            namespace: self.namespace.trim().into(),
            workspace_id: self.workspace_id.trim().into(),
            path,
            summary: self.summary.trim().into(),
            navigation_summary: self.navigation_summary.trim().into(),
            generation: 1,
            content_hash: self.content_hash,
            dirty_children: 0,
            total_children: self.total_children,
            summarized_at: Some(now),
            sensitivity: self.sensitivity,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySource {
    pub id: String,
    pub memory_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_uri: Option<String>,
    pub source_digest: Option<String>,
    pub relationship: String,
    pub source_revision: Option<String>,
    pub observed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewMemorySource {
    pub memory_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub source_uri: Option<String>,
    pub source_digest: Option<String>,
    pub relationship: String,
    pub source_revision: Option<String>,
    pub observed_at: Option<i64>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
}

impl NewMemorySource {
    pub fn into_record(self) -> Result<MemorySource> {
        if self.memory_id.trim().is_empty()
            || self.source_kind.trim().is_empty()
            || self.source_id.trim().is_empty()
            || self.relationship.trim().is_empty()
        {
            return Err(MemoryError::Invalid("memory source identity is incomplete".into()));
        }
        Ok(MemorySource {
            id: format!("src_{}", Uuid::new_v4().simple()),
            memory_id: self.memory_id,
            source_kind: self.source_kind,
            source_id: self.source_id,
            source_uri: self.source_uri,
            source_digest: self.source_digest,
            relationship: self.relationship,
            source_revision: self.source_revision,
            observed_at: self.observed_at.unwrap_or_else(now_epoch),
            valid_from: self.valid_from,
            valid_until: self.valid_until,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChangeSet {
    pub id: String,
    pub workspace_id: String,
    pub origin_type: String,
    pub origin_id: String,
    pub started_at: i64,
    pub committed_at: Option<i64>,
    pub model: Option<String>,
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryChange {
    pub id: String,
    pub change_set_id: String,
    pub memory_id: Option<String>,
    pub operation: String,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub evidence_json: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallTrace {
    pub id: String,
    pub namespace: String,
    pub workspace_id: String,
    pub query: String,
    pub planned_scopes_json: String,
    pub candidates_json: String,
    pub selected_json: String,
    pub token_cost: u64,
    pub semantic_fallback: bool,
    pub elapsed_us: u64,
    pub context_hash: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySchemaDefinition {
    pub schema_id: String,
    pub version: u64,
    pub base_kind: String,
    pub fields_schema_json: String,
    pub retention_policy_json: String,
    pub merge_policy_json: String,
    pub default_sensitivity: Sensitivity,
    pub index_fields_json: String,
    pub updated_at: i64,
}

pub trait SemanticMemoryStore: Send + Sync {
    fn upsert_context_node(&self, node: NewContextNode) -> Result<ContextNode>;
    fn context_node(&self, id: &str) -> Result<Option<ContextNode>>;
    fn search_context_nodes(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ContextNode>>;
    fn mark_context_dirty(&self, id: &str, child_delta: u64) -> Result<Option<ContextNode>>;
    fn link_memory_context(&self, memory_id: &str, context_node_id: &str) -> Result<()>;

    fn record_source(&self, source: NewMemorySource) -> Result<MemorySource>;
    fn sources_for_memory(&self, memory_id: &str) -> Result<Vec<MemorySource>>;

    fn record_change_set(
        &self,
        change_set: &MemoryChangeSet,
        changes: &[MemoryChange],
    ) -> Result<()>;
    fn change_history(&self, workspace_id: &str, limit: usize) -> Result<Vec<MemoryChangeSet>>;

    fn record_recall_trace(&self, trace: &RecallTrace) -> Result<()>;
    fn recent_recall_traces(&self, workspace_id: &str, limit: usize) -> Result<Vec<RecallTrace>>;

    fn register_schema(&self, schema: &MemorySchemaDefinition) -> Result<()>;
    fn list_schemas(&self) -> Result<Vec<MemorySchemaDefinition>>;
}
