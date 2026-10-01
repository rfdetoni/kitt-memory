use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MemoryError, Result, now_epoch};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceOrigin {
    Human,
    Assistant,
    Subagent,
    Tool,
    Repository,
    Memory,
    Skill,
    Plugin,
    Harness,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAssessment {
    pub origin: EvidenceOrigin,
    pub class: String,
    pub priority: u8,
    pub learnable: bool,
}

pub fn assess_evidence(origin: EvidenceOrigin, class: &str) -> EvidenceAssessment {
    let normalized = class.trim().replace('-', "_").to_ascii_uppercase();
    let priority = match normalized.as_str() {
        "USER_CORRECTION" | "USER_EXPLICIT_RULE" => 100,
        "USER_DECISION" => 95,
        "VERIFIED_RESULT" | "VALIDATION_EVIDENCE" => 90,
        "FINAL_RESULT" => 85,
        "REPOSITORY_FACT" => 80,
        "SUBAGENT_RESULT" => 70,
        "ASSISTANT_PROGRESS" => 40,
        "TOOL_RESULT" => 30,
        "ENVIRONMENT_CONTEXT" => 20,
        _ => 10,
    };
    let learnable = !matches!(
        origin,
        EvidenceOrigin::Memory
            | EvidenceOrigin::Skill
            | EvidenceOrigin::Plugin
            | EvidenceOrigin::Harness
            | EvidenceOrigin::System
    );
    EvidenceAssessment {
        origin,
        class: normalized,
        priority,
        learnable,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryConsumptionReceipt {
    pub recall_trace_id: String,
    pub memory_id: String,
    pub consumer: String,
    pub purpose: String,
    pub presented: bool,
    pub referenced: bool,
    pub used_for_action: bool,
    pub outcome: String,
    pub turn_id: String,
    pub consumed_at: i64,
}

impl MemoryConsumptionReceipt {
    pub fn validate(&self) -> Result<()> {
        if self.recall_trace_id.trim().is_empty()
            || self.memory_id.trim().is_empty()
            || self.consumer.trim().is_empty()
            || self.purpose.trim().is_empty()
            || self.turn_id.trim().is_empty()
        {
            return Err(MemoryError::Invalid(
                "memory consumption receipt identity is incomplete".into(),
            ));
        }
        if self.used_for_action && !self.presented {
            return Err(MemoryError::Invalid(
                "memory cannot be used for action unless it was presented".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryJob {
    pub id: String,
    pub phase: String,
    pub source_id: String,
    pub source_revision: String,
    pub source_watermark: String,
    pub status: String,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
    pub attempt: u32,
    pub next_retry_at: Option<i64>,
    pub input_digest: String,
    pub output_digest: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl MemoryJob {
    pub fn new(
        phase: impl Into<String>,
        source_id: impl Into<String>,
        source_revision: impl Into<String>,
        source_watermark: impl Into<String>,
        input_digest: impl Into<String>,
    ) -> Result<Self> {
        let phase = phase.into().trim().to_string();
        let source_id = source_id.into().trim().to_string();
        let source_revision = source_revision.into().trim().to_string();
        let source_watermark = source_watermark.into().trim().to_string();
        let input_digest = input_digest.into().trim().to_string();
        if phase.is_empty()
            || source_id.is_empty()
            || source_revision.is_empty()
            || input_digest.is_empty()
        {
            return Err(MemoryError::Invalid(
                "memory job phase/source/revision/input_digest are required".into(),
            ));
        }
        let now = now_epoch();
        Ok(Self {
            id: format!("mjob_{}", Uuid::new_v4().simple()),
            phase,
            source_id,
            source_revision,
            source_watermark,
            status: "PENDING".into(),
            lease_owner: None,
            lease_until: None,
            attempt: 0,
            next_retry_at: None,
            input_digest,
            output_digest: None,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.phase.trim().is_empty()
            || self.source_id.trim().is_empty()
            || self.source_revision.trim().is_empty()
            || self.input_digest.trim().is_empty()
        {
            return Err(MemoryError::Invalid("invalid memory job identity".into()));
        }
        if !matches!(
            self.status.as_str(),
            "PENDING" | "RUNNING" | "SUCCEEDED" | "FAILED" | "RETRY"
        ) {
            return Err(MemoryError::Invalid("invalid memory job status".into()));
        }
        Ok(())
    }
}
