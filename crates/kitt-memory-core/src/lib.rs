use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub mod baseline;
pub mod knowledge;
pub mod ranking;

pub use baseline::{BaselineEntry, BaselineQuery, MemoryBaseline, build_memory_baseline};
pub use knowledge::{
    CorrectionRecord, KnowledgeEdge, KnowledgeRelation, KnowledgeStore, NewConcept, NewCorrection,
    StoredConcept,
};
pub use ranking::{
    MergeAssessment, MergeCandidate, MergeDisposition, SemanticReranker, SemanticScore,
    assess_merge_candidate, lexical_similarity, retention_score,
};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("invalid memory: {0}")]
    Invalid(String),
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

    pub fn from_db(value: &str) -> Self {
        match value {
            "USER_PREFERENCE" => Self::UserPreference,
            "PROJECT_RULE" => Self::ProjectRule,
            "ARCHITECTURE_DECISION" => Self::ArchitectureDecision,
            "TECHNICAL_FACT" => Self::TechnicalFact,
            "WORKING_PATTERN" => Self::WorkingPattern,
            "FAILED_APPROACH" => Self::FailedApproach,
            "OPEN_ISSUE" => Self::OpenIssue,
            "PROJECT_STATE" => Self::ProjectState,
            "PERSONAL_FACT" => Self::PersonalFact,
            "ROUTINE" => Self::Routine,
            _ => Self::Episodic,
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

    pub fn from_db(value: &str) -> Self {
        match value {
            "public" => Self::Public,
            "personal" => Self::Personal,
            "secret" => Self::Secret,
            "ephemeral" => Self::Ephemeral,
            _ => Self::Private,
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

    pub fn from_db(value: &str) -> Self {
        match value {
            "global" => Self::Global,
            "conversation" => Self::Conversation,
            _ => Self::Workspace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryStatus {
    Active,
    Superseded,
    Archived,
}

impl MemoryStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Superseded => "SUPERSEDED",
            Self::Archived => "ARCHIVED",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "SUPERSEDED" => Self::Superseded,
            "ARCHIVED" => Self::Archived,
            _ => Self::Active,
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
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub namespace: String,
    pub workspace_id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub sensitivity: Sensitivity,
    pub scope: MemoryScope,
    pub importance: f32,
    pub confidence: f32,
    pub pinned: bool,
    pub ttl_seconds: Option<u64>,
    pub metadata_json: String,
}

impl NewMemory {
    pub fn into_record(self) -> Result<MemoryRecord> {
        let content = self.content.trim().to_string();
        if content.is_empty() {
            return Err(MemoryError::Invalid("content is empty".into()));
        }
        if !(0.0..=1.0).contains(&self.importance) || !(0.0..=1.0).contains(&self.confidence) {
            return Err(MemoryError::Invalid(
                "importance/confidence must be 0..1".into(),
            ));
        }
        let now = now_epoch();
        let normalized = normalize(&content);
        Ok(MemoryRecord {
            id: format!("mem_{}", Uuid::new_v4().simple()),
            namespace: self.namespace,
            workspace_id: self.workspace_id,
            kind: self.kind,
            content,
            normalized_content: normalized.clone(),
            status: MemoryStatus::Active,
            sensitivity: self.sensitivity,
            scope: self.scope,
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
            content_hash: hash_normalized(&normalized),
            pinned: self.pinned,
            metadata_json: self.metadata_json,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub namespace: String,
    pub workspace_id: String,
    pub text: String,
    pub limit: usize,
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
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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
