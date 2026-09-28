use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub mod baseline;
pub mod knowledge;
pub mod ranking;
pub mod semantic;

pub use baseline::{BaselineEntry, BaselineQuery, MemoryBaseline, build_memory_baseline};
pub use knowledge::{
    CorrectionRecord, KnowledgeEdge, KnowledgeRelation, KnowledgeStore, NewConcept, NewCorrection,
    StoredConcept,
};
pub use ranking::{
    MergeAssessment, MergeCandidate, MergeDisposition, SemanticReranker, SemanticScore,
    assess_merge_candidate, lexical_similarity, retention_score,
};
pub use semantic::{
    ContextNode, MemoryChange, MemoryChangeSet, MemorySchemaDefinition, MemorySource,
    NewContextNode, NewMemorySource, RecallTrace, SemanticMemoryStore,
};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("invalid memory: {0}")]
    Invalid(String),
    #[error("corrupt memory data: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryKind {
    UserPreference,
    ProjectRule,
    ArchitectureDecision,
    TechnicalFact,
    WorkingPattern,
    FailedApproach,
    OpenIssue,
    ProjectState,
    Episodic,
    PersonalFact,
    Routine,
}

impl MemoryKind {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::UserPreference => "USER_PREFERENCE",
            Self::ProjectRule => "PROJECT_RULE",
            Self::ArchitectureDecision => "ARCHITECTURE_DECISION",
            Self::TechnicalFact => "TECHNICAL_FACT",
            Self::WorkingPattern => "WORKING_PATTERN",
            Self::FailedApproach => "FAILED_APPROACH",
            Self::OpenIssue => "OPEN_ISSUE",
            Self::ProjectState => "PROJECT_STATE",
            Self::Episodic => "EPISODIC",
            Self::PersonalFact => "PERSONAL_FACT",
            Self::Routine => "ROUTINE",
        }
    }

    pub fn from_db(value: &str) -> Result<Self> {
        match value {
            "USER_PREFERENCE" => Ok(Self::UserPreference),
            "PROJECT_RULE" => Ok(Self::ProjectRule),
            "ARCHITECTURE_DECISION" => Ok(Self::ArchitectureDecision),
            "TECHNICAL_FACT" => Ok(Self::TechnicalFact),
            "WORKING_PATTERN" => Ok(Self::WorkingPattern),
            "FAILED_APPROACH" => Ok(Self::FailedApproach),
            "OPEN_ISSUE" => Ok(Self::OpenIssue),
            "PROJECT_STATE" => Ok(Self::ProjectState),
            "EPISODIC" => Ok(Self::Episodic),
            "PERSONAL_FACT" => Ok(Self::PersonalFact),
            "ROUTINE" => Ok(Self::Routine),
            other => Err(MemoryError::Corrupt(format!(
                "unknown memory kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Personal,
    Private,
    Secret,
    Ephemeral,
}

impl Sensitivity {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Personal => "personal",
            Self::Private => "private",
            Self::Secret => "secret",
            Self::Ephemeral => "ephemeral",
        }
    }

    pub fn from_db(value: &str) -> Result<Self> {
        match value {
            "public" => Ok(Self::Public),
            "personal" => Ok(Self::Personal),
            "private" => Ok(Self::Private),
            "secret" => Ok(Self::Secret),
            "ephemeral" => Ok(Self::Ephemeral),
            other => Err(MemoryError::Corrupt(format!(
                "unknown sensitivity: {other}"
            ))),
        }
    }

    pub fn restriction_rank(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Personal => 1,
            Self::Private => 2,
            Self::Secret => 3,
            Self::Ephemeral => 4,
        }
    }

    pub fn most_restrictive(self, other: Self) -> Self {
        if self.restriction_rank() >= other.restriction_rank() {
            self
        } else {
            other
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Global,
    Workspace,
    Conversation,
}

impl MemoryScope {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Conversation => "conversation",
        }
    }

    pub fn from_db(value: &str) -> Result<Self> {
        match value {
            "global" => Ok(Self::Global),
            "workspace" => Ok(Self::Workspace),
            "conversation" => Ok(Self::Conversation),
            other => Err(MemoryError::Corrupt(format!(
                "unknown memory scope: {other}"
            ))),
        }
    }

    pub fn requires_scope_key(&self) -> bool {
        matches!(self, Self::Conversation)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryStatus {
    Active,
    Superseded,
    Archived,
    Candidate,
}

impl MemoryStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Superseded => "SUPERSEDED",
            Self::Archived => "ARCHIVED",
            Self::Candidate => "CANDIDATE",
        }
    }

    pub fn from_db(value: &str) -> Result<Self> {
        match value {
            "ACTIVE" => Ok(Self::Active),
            "SUPERSEDED" => Ok(Self::Superseded),
            "ARCHIVED" => Ok(Self::Archived),
            "CANDIDATE" => Ok(Self::Candidate),
            other => Err(MemoryError::Corrupt(format!(
                "unknown memory status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub namespace: String,
    pub workspace_id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub normalized_content: String,
    pub status: MemoryStatus,
    pub sensitivity: Sensitivity,
    pub scope: MemoryScope,
    #[serde(default)]
    pub scope_key: Option<String>,
    pub importance: f32,
    pub confidence: f32,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_accessed_at: Option<i64>,
    pub access_count: u64,
    #[serde(default)]
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub supersedes_id: Option<String>,
    pub content_hash: String,
    pub pinned: bool,
    pub metadata_json: String,
}

impl MemoryRecord {
    pub fn is_valid_at(&self, at: i64) -> bool {
        self.valid_from.is_none_or(|from| from <= at)
            && self.valid_until.is_none_or(|until| until > at)
    }

    pub fn canonicalized_for_storage(&self) -> Result<Self> {
        let mut record = self.clone();
        record.namespace = record.namespace.trim().to_string();
        record.workspace_id = record.workspace_id.trim().to_string();
        record.content = record.content.trim().to_string();
        record.scope_key = record
            .scope_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        validate_identity(&record.namespace, &record.workspace_id)?;
        validate_scores(record.importance, record.confidence)?;
        validate_metadata(&record.metadata_json)?;
        if record.content.is_empty() {
            return Err(MemoryError::Invalid("content is empty".into()));
        }
        if record.scope.requires_scope_key() && record.scope_key.is_none() {
            return Err(MemoryError::Invalid(
                "conversation memories require scope_key".into(),
            ));
        }
        if record.scope != MemoryScope::Conversation {
            record.scope_key = None;
        }
        if record.scope == MemoryScope::Global {
            record.workspace_id = "global".into();
        }
        if record
            .valid_from
            .zip(record.valid_until)
            .is_some_and(|(from, until)| until < from)
        {
            return Err(MemoryError::Invalid(
                "valid_until cannot be before valid_from".into(),
            ));
        }
        if record.access_count > i64::MAX as u64 {
            return Err(MemoryError::Invalid(
                "access_count exceeds SQLite range".into(),
            ));
        }
        record.normalized_content = normalize(&record.content);
        record.content_hash = hash_normalized(&record.normalized_content);
        Ok(record)
    }
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub namespace: String,
    pub workspace_id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub sensitivity: Sensitivity,
    pub scope: MemoryScope,
    pub scope_key: Option<String>,
    pub importance: f32,
    pub confidence: f32,
    pub pinned: bool,
    pub ttl_seconds: Option<u64>,
    pub metadata_json: String,
}

impl NewMemory {
    pub fn into_record(self) -> Result<MemoryRecord> {
        let now = now_epoch();
        MemoryRecord {
            id: format!("mem_{}", Uuid::new_v4().simple()),
            namespace: self.namespace,
            workspace_id: self.workspace_id,
            kind: self.kind,
            content: self.content,
            normalized_content: String::new(),
            status: MemoryStatus::Active,
            sensitivity: self.sensitivity,
            scope: self.scope,
            scope_key: self.scope_key,
            importance: self.importance,
            confidence: self.confidence,
            created_at: now,
            updated_at: now,
            last_accessed_at: None,
            access_count: 0,
            valid_from: Some(now),
            valid_until: self
                .ttl_seconds
                .map(|ttl| now.saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX))),
            supersedes_id: None,
            content_hash: String::new(),
            pinned: self.pinned,
            metadata_json: self.metadata_json,
        }
        .canonicalized_for_storage()
    }
}

#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub namespace: String,
    pub workspace_id: String,
    pub scope_key: Option<String>,
    pub text: String,
    pub limit: usize,
    pub as_of: Option<i64>,
    pub allow_private: bool,
    pub allow_secret: bool,
}

pub trait MemoryStore: Send + Sync {
    fn remember(&self, memory: NewMemory) -> Result<MemoryRecord>;
    fn upsert_record(&self, memory: &MemoryRecord) -> Result<()>;
    fn recall(&self, query: &RecallQuery) -> Result<Vec<MemoryRecord>>;

    fn recall_with_reranker(
        &self,
        query: &RecallQuery,
        _reranker: &dyn SemanticReranker,
    ) -> Result<Vec<MemoryRecord>> {
        self.recall(query)
    }

    fn baseline(&self, _query: &BaselineQuery) -> Result<MemoryBaseline> {
        Err(MemoryError::Invalid(
            "baseline snapshots are not implemented by this store".into(),
        ))
    }

    fn find_merge_candidates(
        &self,
        _memory: &NewMemory,
        _limit: usize,
        _reranker: Option<&dyn SemanticReranker>,
    ) -> Result<Vec<MergeCandidate>> {
        Ok(Vec::new())
    }

    fn forget(&self, id: &str) -> Result<bool>;
    fn set_status(
        &self,
        id: &str,
        status: MemoryStatus,
        supersedes_id: Option<&str>,
    ) -> Result<bool>;
    fn prune_expired(&self) -> Result<usize>;
    fn consolidate_exact_duplicates(&self, namespace: &str, workspace_id: &str) -> Result<usize>;
}

#[derive(Debug, Clone, Copy)]
pub struct EgressPolicy {
    pub is_local_provider: bool,
    pub allow_personal_remote: bool,
}

impl EgressPolicy {
    pub fn allows(self, sensitivity: Sensitivity) -> bool {
        match sensitivity {
            Sensitivity::Ephemeral | Sensitivity::Secret | Sensitivity::Private => {
                self.is_local_provider
            }
            Sensitivity::Personal => self.is_local_provider || self.allow_personal_remote,
            Sensitivity::Public => true,
        }
    }
}

pub fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut separator = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            separator = false;
        } else if !out.is_empty() && !separator {
            out.push(' ');
            separator = true;
        }
    }
    out.trim().to_string()
}

fn validate_identity(namespace: &str, workspace_id: &str) -> Result<()> {
    if namespace.is_empty() {
        return Err(MemoryError::Invalid("namespace is empty".into()));
    }
    if workspace_id.is_empty() {
        return Err(MemoryError::Invalid("workspace_id is empty".into()));
    }
    Ok(())
}

fn validate_scores(importance: f32, confidence: f32) -> Result<()> {
    if !importance.is_finite()
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&importance)
        || !(0.0..=1.0).contains(&confidence)
    {
        return Err(MemoryError::Invalid(
            "importance/confidence must be finite values in 0..1".into(),
        ));
    }
    Ok(())
}

fn validate_metadata(metadata_json: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| MemoryError::Invalid(format!("metadata_json is invalid: {error}")))?;
    if !value.is_object() {
        return Err(MemoryError::Invalid(
            "metadata_json must be a JSON object".into(),
        ));
    }
    Ok(())
}

pub fn hash_normalized(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_leaves_local() {
        assert!(
            !EgressPolicy {
                is_local_provider: false,
                allow_personal_remote: true
            }
            .allows(Sensitivity::Secret)
        );
    }

    #[test]
    fn normalization_is_stable() {
        assert_eq!("hello world", normalize("  Hello   WORLD "));
    }

    #[test]
    fn huge_ttl_saturates_instead_of_wrapping() {
        let record = NewMemory {
            namespace: "test".into(),
            workspace_id: "workspace".into(),
            kind: MemoryKind::TechnicalFact,
            content: "fact".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: Some(u64::MAX),
            metadata_json: "{}".into(),
        }
        .into_record()
        .unwrap();
        assert_eq!(record.valid_until, Some(i64::MAX));
    }

    #[test]
    fn sensitivity_is_monotonic() {
        assert_eq!(
            Sensitivity::Secret,
            Sensitivity::Public.most_restrictive(Sensitivity::Secret)
        );
        assert_eq!(
            Sensitivity::Private,
            Sensitivity::Private.most_restrictive(Sensitivity::Personal)
        );
    }
}
