use serde::{Deserialize, Serialize};

use crate::{
    MemoryKind, MemoryRecord, MemoryScope, MemoryStatus, Sensitivity, estimate_tokens, now_epoch,
};

#[derive(Debug, Clone)]
pub struct BaselineQuery {
    pub namespace: String,
    pub workspace_id: String,
    pub scope_key: Option<String>,
    pub max_tokens: usize,
    pub as_of: Option<i64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

fn allowed(memory: &MemoryRecord, query: &BaselineQuery, now: i64) -> bool {
    let scope_allowed = match memory.scope {
        MemoryScope::Global => true,
        MemoryScope::Workspace => memory.workspace_id == query.workspace_id,
        MemoryScope::Conversation => {
            query.scope_key.is_some()
                && memory.workspace_id == query.workspace_id
                && memory.scope_key.as_deref() == query.scope_key.as_deref()
        }
    };
    if memory.status != MemoryStatus::Active
        || memory.namespace != query.namespace
        || !scope_allowed
        || !memory.is_valid_at(now)
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

fn truncate_at_boundary(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let hard = value
        .char_indices()
        .nth(max_chars.saturating_sub(1))
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let prefix = &value[..hard];
    let boundary = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| matches!(ch, '.' | '!' | '?' | '\n'))
        .map(|(index, ch)| index + ch.len_utf8())
        .filter(|index| *index >= hard / 2)
        .unwrap_or(hard);
    format!("{}…", prefix[..boundary].trim_end())
}

fn stable_score(memory: &MemoryRecord) -> f32 {
    let kind_bonus = match memory.kind {
        MemoryKind::ProjectRule | MemoryKind::ArchitectureDecision => 0.20,
        MemoryKind::UserPreference => 0.15,
        MemoryKind::TechnicalFact | MemoryKind::WorkingPattern => 0.08,
        _ => 0.0,
    };
    let updated_day = memory.updated_at.div_euclid(86_400).max(0) as f32;
    memory.importance.clamp(0.0, 1.0) * 0.52
        + memory.confidence.clamp(0.0, 1.0) * 0.28
        + if memory.pinned { 0.45 } else { 0.0 }
        + kind_bonus
        + (updated_day.min(100_000.0) / 100_000.0) * 0.01
}

pub fn build_memory_baseline(
    memories: impl IntoIterator<Item = MemoryRecord>,
    query: &BaselineQuery,
) -> MemoryBaseline {
    let now = query.as_of.unwrap_or_else(now_epoch);
    let max_tokens = query.max_tokens.clamp(32, 16_384);
    let max_chars = max_tokens.saturating_mul(4);
    let per_entry_chars = (max_chars / 3).clamp(120, 1_200);

    let mut candidates = memories
        .into_iter()
        .filter(|memory| allowed(memory, query, now))
        .map(|memory| {
            let (label, rank) = section(&memory.kind);
            let score = stable_score(&memory);
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
    let mut used_tokens = 0usize;

    for (memory, label, _, score) in candidates {
        let content = truncate_at_boundary(&memory.content, per_entry_chars);
        let cost_tokens = estimate_tokens(&content)
            .saturating_add(estimate_tokens(label))
            .saturating_add(2);
        if used_tokens.saturating_add(cost_tokens) > max_tokens {
            continue;
        }
        used_tokens = used_tokens.saturating_add(cost_tokens);
        used_chars = used_chars
            .saturating_add(content.chars().count())
            .saturating_add(label.len());
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
    let estimated_tokens = used_tokens;
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
        baseline_revision: None,
        etag: None,
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
            scope_key: None,
            importance: 0.8,
            confidence: 1.0,
            created_at: 1,
            updated_at: 1,
            last_accessed_at: None,
            access_count: 0,
            valid_from: Some(1),
            valid_until: None,
            supersedes_id: None,
            content_hash: String::new(),
            pinned,
            gist: String::new(),
            tokens_est: 0,
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
                scope_key: None,
                max_tokens: 80,
                as_of: None,
                allow_private: true,
                allow_secret: false,
            },
        );
        assert_eq!(result.entries[0].memory_id, "a");
        assert!(result.dropped_count > 0);
        assert!(result.budget_pressure > 0.0);
    }

    #[test]
    fn baseline_order_does_not_depend_on_access_history_or_wall_clock() {
        let mut a = item("a", MemoryKind::TechnicalFact, "alpha", false);
        let mut b = item("b", MemoryKind::TechnicalFact, "beta", false);
        a.access_count = 999;
        a.last_accessed_at = Some(9_999_999);
        b.access_count = 0;
        b.last_accessed_at = None;
        let query = BaselineQuery {
            namespace: "agent".into(),
            workspace_id: "ws".into(),
            scope_key: None,
            max_tokens: 200,
            as_of: Some(10_000_000),
            allow_private: true,
            allow_secret: false,
        };
        let first = build_memory_baseline(vec![a.clone(), b.clone()], &query);
        a.access_count = 0;
        a.last_accessed_at = None;
        b.access_count = 999;
        b.last_accessed_at = Some(10_000_000);
        let second = build_memory_baseline(vec![a, b], &query);
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| &entry.memory_id)
                .collect::<Vec<_>>(),
            second
                .entries
                .iter()
                .map(|entry| &entry.memory_id)
                .collect::<Vec<_>>()
        );
    }
}
