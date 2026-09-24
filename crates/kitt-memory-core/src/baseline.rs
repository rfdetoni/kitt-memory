use serde::{Deserialize, Serialize};

use crate::{
    MemoryKind, MemoryRecord, MemoryScope, MemoryStatus, Sensitivity, now_epoch,
    ranking::retention_score,
};

#[derive(Debug, Clone)]
pub struct BaselineQuery {
    pub namespace: String,
    pub workspace_id: String,
    pub max_tokens: usize,
    pub allow_private: bool,
    pub allow_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub memory_id: String,
    pub section: String,
    pub content: String,
    pub pinned: bool,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBaseline {
    pub entries: Vec<BaselineEntry>,
    pub estimated_tokens: usize,
    pub max_tokens: usize,
    pub dropped_count: usize,
    pub budget_pressure: f32,
}

fn allowed(memory: &MemoryRecord, query: &BaselineQuery, now: i64) -> bool {
    if memory.status != MemoryStatus::Active
        || memory.namespace != query.namespace
        || (memory.workspace_id != query.workspace_id && memory.scope != MemoryScope::Global)
        || memory.valid_until.is_some_and(|until| until <= now)
    {
        return false;
    }
    match memory.sensitivity {
        Sensitivity::Secret => query.allow_secret,
        Sensitivity::Private => query.allow_private,
        Sensitivity::Ephemeral => false,
        _ => true,
    }
}

fn section(kind: &MemoryKind) -> (&'static str, u8) {
    match kind {
        MemoryKind::UserPreference | MemoryKind::PersonalFact | MemoryKind::Routine => {
            ("User baseline", 0)
        }
        MemoryKind::ProjectRule | MemoryKind::WorkingPattern => ("Project guardrails", 1),
        MemoryKind::ArchitectureDecision | MemoryKind::TechnicalFact => {
            ("Architecture knowledge", 2)
        }
        MemoryKind::OpenIssue | MemoryKind::ProjectState | MemoryKind::FailedApproach => {
            ("Current project state", 3)
        }
        MemoryKind::Episodic => ("Recent durable context", 4),
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", value.chars().take(keep).collect::<String>())
}

pub fn build_memory_baseline(
    memories: impl IntoIterator<Item = MemoryRecord>,
    query: &BaselineQuery,
) -> MemoryBaseline {
    let now = now_epoch();
    let max_tokens = query.max_tokens.clamp(32, 16_384);
    let max_chars = max_tokens.saturating_mul(4);
    let per_entry_chars = (max_chars / 3).clamp(120, 1_200);

    let mut candidates = memories
        .into_iter()
        .filter(|memory| allowed(memory, query, now))
        .map(|memory| {
            let (label, rank) = section(&memory.kind);
            let score = retention_score(&memory, now)
                + if memory.pinned { 0.4 } else { 0.0 }
                + match memory.kind {
                    MemoryKind::ProjectRule | MemoryKind::ArchitectureDecision => 0.2,
                    MemoryKind::UserPreference => 0.15,
                    _ => 0.0,
                };
            (memory, label, rank, score)
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .0
            .pinned
            .cmp(&left.0.pinned)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| right.3.total_cmp(&left.3))
            .then_with(|| right.0.updated_at.cmp(&left.0.updated_at))
            .then_with(|| left.0.id.cmp(&right.0.id))
    });

    let eligible = candidates.len();
    let mut entries = Vec::new();
    let mut used_chars = 0usize;

    for (memory, label, _, score) in candidates {
        let content = truncate_chars(&memory.content, per_entry_chars);
        let cost = content.chars().count().saturating_add(label.len()).saturating_add(8);
        if !entries.is_empty() && used_chars.saturating_add(cost) > max_chars {
            continue;
        }
        used_chars = used_chars.saturating_add(cost);
        entries.push(BaselineEntry {
            memory_id: memory.id,
            section: label.into(),
            content,
            pinned: memory.pinned,
            score,
        });
        if used_chars >= max_chars {
            break;
        }
    }

    let dropped_count = eligible.saturating_sub(entries.len());
    let estimated_tokens = (used_chars.saturating_add(3)) / 4;
    let pressure = if max_tokens == 0 {
        1.0
    } else {
        estimated_tokens as f32 / max_tokens as f32
    };

    MemoryBaseline {
        entries,
        estimated_tokens,
        max_tokens,
        dropped_count,
        budget_pressure: pressure.min(1.5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryScope, Sensitivity, normalize};

    fn item(id: &str, kind: MemoryKind, content: &str, pinned: bool) -> MemoryRecord {
        MemoryRecord {
            id: id.into(),
            namespace: "agent".into(),
            workspace_id: "ws".into(),
            kind,
            content: content.into(),
            normalized_content: normalize(content),
            status: MemoryStatus::Active,
            sensitivity: Sensitivity::Private,
            scope: MemoryScope::Workspace,
            importance: 0.8,
            confidence: 1.0,
            created_at: 1,
            updated_at: 1,
            last_accessed_at: None,
            access_count: 0,
            valid_until: None,
            supersedes_id: None,
            content_hash: String::new(),
            pinned,
            metadata_json: "{}".into(),
        }
    }

    #[test]
    fn baseline_is_bounded_and_reports_pressure() {
        let memories = vec![
            item("b", MemoryKind::TechnicalFact, &"x".repeat(900), false),
            item("a", MemoryKind::ProjectRule, "always test changes", true),
            item("c", MemoryKind::OpenIssue, &"y".repeat(900), false),
        ];
        let result = build_memory_baseline(
            memories,
            &BaselineQuery {
                namespace: "agent".into(),
                workspace_id: "ws".into(),
                max_tokens: 80,
                allow_private: true,
                allow_secret: false,
            },
        );
        assert_eq!(result.entries[0].memory_id, "a");
        assert!(result.dropped_count > 0);
        assert!(result.budget_pressure > 0.0);
    }
}
