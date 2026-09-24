use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MemoryError, Result, now_epoch};

#[derive(Debug, Clone)]
pub struct NewCorrection {
    pub namespace: String,
    pub workspace_id: String,
    pub context: String,
    pub predicted: String,
    pub corrected: String,
    pub reason: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionRecord {
    pub id: String,
    pub namespace: String,
    pub workspace_id: String,
    pub context: String,
    pub predicted: String,
    pub corrected: String,
    pub reason: Option<String>,
    pub source: String,
    pub applied_count: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NewCorrection {
    pub fn into_record(self) -> Result<CorrectionRecord> {
        let context = self.context.trim().to_string();
        let predicted = self.predicted.trim().to_string();
        let corrected = self.corrected.trim().to_string();
        if context.is_empty() || predicted.is_empty() || corrected.is_empty() {
            return Err(MemoryError::Invalid(
                "correction context, predicted and corrected values are required".into(),
            ));
        }
        let now = now_epoch();
        Ok(CorrectionRecord {
            id: format!("corr_{}", Uuid::new_v4().simple()),
            namespace: self.namespace,
            workspace_id: self.workspace_id,
            context,
            predicted,
            corrected,
            reason: self.reason.filter(|value| !value.trim().is_empty()),
            source: if self.source.trim().is_empty() {
                "agent".into()
            } else {
                self.source.trim().to_string()
            },
            applied_count: 0,
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewConcept {
    pub namespace: String,
    pub workspace_id: String,
    pub name: String,
    pub definition: String,
    pub confidence: f32,
    pub labels: Vec<String>,
    pub source_memory_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredConcept {
    pub id: String,
    pub namespace: String,
    pub workspace_id: String,
    pub name: String,
    pub definition: String,
    pub confidence: f32,
    pub revision: u64,
    pub labels: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl NewConcept {
    pub fn into_record(self) -> Result<StoredConcept> {
        let name = self.name.trim().to_string();
        let definition = self.definition.trim().to_string();
        if name.is_empty() || definition.is_empty() {
            return Err(MemoryError::Invalid(
                "concept name and definition are required".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(MemoryError::Invalid(
                "concept confidence must be 0..1".into(),
            ));
        }
        let now = now_epoch();
        Ok(StoredConcept {
            id: format!("concept_{}", Uuid::new_v4().simple()),
            namespace: self.namespace,
            workspace_id: self.workspace_id,
            name,
            definition,
            confidence: self.confidence,
            revision: 1,
            labels: dedupe(self.labels),
            source_memory_ids: dedupe(self.source_memory_ids),
            created_at: now,
            updated_at: now,
        })
    }
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut out = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRelation {
    Supports,
    Requires,
    Conflicts,
    Refines,
    Replaces,
    Related,
}

impl KnowledgeRelation {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Supports => "SUPPORTS",
            Self::Requires => "REQUIRES",
            Self::Conflicts => "CONFLICTS",
            Self::Refines => "REFINES",
            Self::Replaces => "REPLACES",
            Self::Related => "RELATED",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "SUPPORTS" => Self::Supports,
            "REQUIRES" => Self::Requires,
            "CONFLICTS" => Self::Conflicts,
            "REFINES" => Self::Refines,
            "REPLACES" => Self::Replaces,
            _ => Self::Related,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub id: String,
    pub namespace: String,
    pub workspace_id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation: KnowledgeRelation,
    pub weight: f32,
    pub created_at: i64,
}

pub trait KnowledgeStore: Send + Sync {
    fn record_correction(&self, correction: NewCorrection) -> Result<CorrectionRecord>;
    fn search_corrections(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CorrectionRecord>>;
    fn mark_correction_applied(&self, id: &str) -> Result<bool>;

    fn upsert_concept(&self, concept: NewConcept) -> Result<StoredConcept>;
    fn search_concepts(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredConcept>>;
    fn link_concepts(
        &self,
        namespace: &str,
        workspace_id: &str,
        source_id: &str,
        target_id: &str,
        relation: KnowledgeRelation,
        weight: f32,
    ) -> Result<KnowledgeEdge>;
    fn links_for_concept(
        &self,
        namespace: &str,
        workspace_id: &str,
        concept_id: &str,
    ) -> Result<Vec<KnowledgeEdge>>;

    fn expand_concepts(
        &self,
        _namespace: &str,
        _workspace_id: &str,
        _seed_ids: &[String],
        _max_hops: usize,
        _limit: usize,
    ) -> Result<Vec<StoredConcept>> {
        Ok(Vec::new())
    }

    fn search_concept_neighborhood(
        &self,
        namespace: &str,
        workspace_id: &str,
        query: &str,
        max_hops: usize,
        limit: usize,
    ) -> Result<Vec<StoredConcept>> {
        let seed_limit = limit.clamp(1, 100).min(8);
        let seeds = self.search_concepts(namespace, workspace_id, query, seed_limit)?;
        let seed_ids = seeds
            .iter()
            .map(|concept| concept.id.clone())
            .collect::<Vec<_>>();
        self.expand_concepts(
            namespace,
            workspace_id,
            &seed_ids,
            max_hops.clamp(0, 4),
            limit.clamp(1, 100),
        )
    }
}
