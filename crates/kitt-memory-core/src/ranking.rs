use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{MemoryKind, MemoryRecord, NewMemory, Result, normalize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticScore {
    pub memory_id: String,
    pub score: f32,
}

pub trait SemanticReranker: Send + Sync {
    fn score(&self, query: &str, candidates: &[MemoryRecord]) -> Result<Vec<SemanticScore>>;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeDisposition {
    Distinct,
    NeedsReview,
    Equivalent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeAssessment {
    pub disposition: MergeDisposition,
    pub score: f32,
    pub lexical_score: f32,
    pub semantic_score: Option<f32>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeCandidate {
    pub memory: MemoryRecord,
    pub assessment: MergeAssessment,
}

pub fn lexical_terms(query: &str) -> HashSet<String> {
    normalize(query)
        .split_whitespace()
        .filter(|term| term.len() >= 2)
        .map(str::to_owned)
        .collect()
}

pub fn lexical_similarity(query: &str, memory: &MemoryRecord) -> f32 {
    let query_terms = lexical_terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let memory_terms = lexical_terms(&memory.normalized_content);
    if memory_terms.is_empty() {
        return 0.0;
    }
    let overlap = query_terms.intersection(&memory_terms).count() as f32;
    let union = query_terms.union(&memory_terms).count() as f32;
    if union == 0.0 { 0.0 } else { overlap / union }
}

pub fn retention_score(memory: &MemoryRecord, now: i64) -> f32 {
    let anchor = memory
        .last_accessed_at
        .unwrap_or(memory.updated_at)
        .max(memory.updated_at);
    let age_days = ((now - anchor).max(0) as f32) / 86_400.0;
    let freshness = 1.0 / (1.0 + age_days / 45.0);
    let usage = ((memory.access_count as f32 + 1.0).ln() / 5.0).min(1.0);
    let mut score = memory.importance.clamp(0.0, 1.0) * 0.34
        + memory.confidence.clamp(0.0, 1.0) * 0.24
        + freshness * 0.27
        + usage * 0.15;
    if memory.pinned {
        score += 0.25;
    }
    score
}

pub fn lexical_score_with_terms(
    terms: &HashSet<String>,
    memory: &MemoryRecord,
    now: i64,
) -> f32 {
    let overlap = terms
        .iter()
        .filter(|term| {
            memory
                .normalized_content
                .split_whitespace()
                .any(|word| word == term.as_str())
        })
        .count() as f32;
    overlap * 1.5 + retention_score(memory, now) * 4.0
}

pub fn lexical_score(query: &str, memory: &MemoryRecord, now: i64) -> f32 {
    lexical_score_with_terms(&lexical_terms(query), memory, now)
}

fn negation_mismatch(left: &str, right: &str) -> bool {
    const NEGATIONS: [&str; 6] = ["not", "never", "no", "without", "disable", "avoid"];
    let l = lexical_terms(left);
    let r = lexical_terms(right);
    NEGATIONS
        .iter()
        .any(|word| l.contains(*word) != r.contains(*word))
}

pub fn assess_merge_candidate(
    existing: &MemoryRecord,
    incoming: &NewMemory,
    semantic_score: Option<f32>,
) -> MergeAssessment {
    let normalized = normalize(&incoming.content);
    if existing.normalized_content == normalized {
        return MergeAssessment {
            disposition: MergeDisposition::Equivalent,
            score: 1.0,
            lexical_score: 1.0,
            semantic_score,
            reason: "exact normalized content".into(),
        };
    }

    let lexical = lexical_similarity(&incoming.content, existing);
    let semantic = semantic_score.map(|score| score.clamp(0.0, 1.0));
    let blended = match semantic {
        Some(value) => (lexical * 0.45) + (value * 0.55),
        None => lexical,
    };
    let same_kind = existing.kind == incoming.kind;
    let polarity_changed = negation_mismatch(&existing.content, &incoming.content);

    let (disposition, reason) = if same_kind
        && !polarity_changed
        && lexical >= 0.90
        && blended >= 0.94
    {
        (
            MergeDisposition::Equivalent,
            "high-confidence same-kind paraphrase",
        )
    } else if blended >= 0.70 || lexical >= 0.65 {
        (
            MergeDisposition::NeedsReview,
            if polarity_changed {
                "similar content with a possible polarity change"
            } else if !same_kind {
                "similar content with a different memory kind"
            } else {
                "similar content requires consolidation review"
            },
        )
    } else {
        (MergeDisposition::Distinct, "insufficient similarity")
    };

    MergeAssessment {
        disposition,
        score: blended,
        lexical_score: lexical,
        semantic_score: semantic,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryScope, MemoryStatus, Sensitivity};

    fn record(content: &str) -> MemoryRecord {
        MemoryRecord {
            id: "m".into(),
            namespace: "agent".into(),
            workspace_id: "w".into(),
            kind: MemoryKind::ProjectRule,
            content: content.into(),
            normalized_content: normalize(content),
            status: MemoryStatus::Active,
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.8,
            confidence: 0.9,
            created_at: 1,
            updated_at: 1,
            last_accessed_at: None,
            access_count: 0,
            valid_until: None,
            supersedes_id: None,
            content_hash: String::new(),
            pinned: false,
            metadata_json: "{}".into(),
        }
    }

    #[test]
    fn retention_rewards_recent_access_without_making_it_permanent() {
        let mut old = record("rule");
        old.updated_at = 1;
        let mut accessed = old.clone();
        accessed.last_accessed_at = Some(1_000_000);
        accessed.access_count = 8;
        assert!(retention_score(&accessed, 1_000_100) > retention_score(&old, 1_000_100));
    }

    #[test]
    fn near_duplicate_with_negation_requires_review() {
        let existing = record("always run tests before commit");
        let incoming = NewMemory {
            namespace: "agent".into(),
            workspace_id: "w".into(),
            kind: MemoryKind::ProjectRule,
            content: "never run tests before commit".into(),
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.8,
            confidence: 1.0,
            pinned: false,
            ttl_seconds: None,
            metadata_json: "{}".into(),
        };
        let assessment = assess_merge_candidate(&existing, &incoming, Some(0.97));
        assert_eq!(assessment.disposition, MergeDisposition::NeedsReview);
    }
}
