use kitt_memory_core::{
    DreamRunRecord, EvidenceOrigin, KnowledgeRelation, KnowledgeStore, MemoryConsumptionReceipt,
    MemoryJob, MemoryRecord, MemorySource, MemoryStore, NewConcept, NewCorrection, Sensitivity,
    assess_evidence, hash_normalized,
};
use kitt_memory_sqlite::SqliteMemoryStore;
use serde_json::{Value, json};

use super::{as_str, parse_sensitivity, parse_status};

pub(super) fn handle(store: &SqliteMemoryStore, payload: &Value) -> Result<Value, String> {
    let operation = as_str(payload, "operation")?;
    let args = payload
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match operation {
        "request.status" => {
            let id = as_str(&args, "request_id")?;
            if id.is_empty() || id.len() > 256 {
                return Err("invalid request_id".into());
            }
            let saved = store.request_status(id).map_err(|e| e.to_string())?;
            Ok(match saved {
                None => json!({"state":"not_found"}),
                Some(value) if value.is_empty() => json!({"state":"outcome_unknown"}),
                Some(value) => {
                    json!({"state":"completed", "response":serde_json::from_str::<Value>(&value).map_err(|e| e.to_string())?})
                }
            })
        }
        "list" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace = as_str(&args, "workspace_id")?;
            let status = args
                .get("status")
                .and_then(Value::as_str)
                .map(parse_status)
                .transpose()?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(512) as usize;
            let records = store
                .list_records(namespace, workspace, status, limit)
                .map_err(|e| e.to_string())?;
            Ok(json!({"records": records}))
        }
        "get" => {
            let record = store
                .get_record(as_str(&args, "id")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({"record": record}))
        }
        "set_status" => {
            let changed = store
                .set_status(
                    as_str(&args, "id")?,
                    parse_status(as_str(&args, "status")?)?,
                    args.get("supersedes_id").and_then(Value::as_str),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"changed": changed}))
        }
        "pin" => {
            let changed = store
                .set_pinned(
                    as_str(&args, "id")?,
                    args.get("pinned").and_then(Value::as_bool).unwrap_or(true),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"changed": changed}))
        }
        "touch" => {
            let ids = args
                .get("ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            store.touch_records(&ids).map_err(|e| e.to_string())?;
            Ok(json!({"touched": ids.len()}))
        }
        "archive_workspace" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace = as_str(&args, "workspace_id")?;
            let records = store
                .archive_workspace(namespace, workspace)
                .map_err(|e| e.to_string())?;
            Ok(json!({"records": records}))
        }
        "correction.record" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace_id = as_str(&args, "workspace_id")?;
            let sensitivity = args
                .get("sensitivity")
                .and_then(Value::as_str)
                .map(parse_sensitivity)
                .transpose()?
                .unwrap_or(Sensitivity::Private);
            let source_memory_ids = args
                .get("source_memory_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            let correction = store
                .record_correction(NewCorrection {
                    namespace: namespace.to_string(),
                    workspace_id: workspace_id.to_string(),
                    context: as_str(&args, "context")?.to_string(),
                    predicted: as_str(&args, "predicted")?.to_string(),
                    corrected: as_str(&args, "corrected")?.to_string(),
                    reason: args
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    source: args
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or("agent")
                        .to_string(),
                    sensitivity,
                    source_memory_ids,
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({"correction": correction}))
        }
        "concept.upsert" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace_id = as_str(&args, "workspace_id")?;
            let sensitivity = args
                .get("sensitivity")
                .and_then(Value::as_str)
                .map(parse_sensitivity)
                .transpose()?
                .unwrap_or(Sensitivity::Private);
            let labels = args
                .get("labels")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            let source_memory_ids = args
                .get("source_memory_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            let concept = store
                .upsert_concept(NewConcept {
                    namespace: namespace.to_string(),
                    workspace_id: workspace_id.to_string(),
                    name: as_str(&args, "name")?.to_string(),
                    definition: as_str(&args, "definition")?.to_string(),
                    confidence: args
                        .get("confidence")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.7) as f32,
                    sensitivity,
                    labels,
                    source_memory_ids,
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({"concept": concept}))
        }
        "concept.link" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace_id = as_str(&args, "workspace_id")?;
            let normalized = as_str(&args, "relation")?
                .trim()
                .replace('-', "_")
                .to_ascii_uppercase();
            let relation_name = match normalized.as_str() {
                "RELATED_TO" => "RELATED",
                "DEPENDS_ON" => "REQUIRES",
                "CONTRADICTS" => "CONFLICTS",
                "SUPERSEDES" => "REPLACES",
                other => other,
            };
            let relation = KnowledgeRelation::from_db(relation_name).map_err(|e| e.to_string())?;
            let edge = store
                .link_concepts(
                    namespace,
                    workspace_id,
                    as_str(&args, "source_id")?,
                    as_str(&args, "target_id")?,
                    relation,
                    args.get("weight").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"edge": edge}))
        }
        "dream.last" => {
            let run = store
                .last_dream_run(as_str(&args, "workspace_id")?)
                .map_err(|e| e.to_string())?;
            Ok(json!({"run": run}))
        }
        "dream.record" => {
            let run: DreamRunRecord =
                serde_json::from_value(args.get("run").cloned().ok_or("missing run")?)
                    .map_err(|e| e.to_string())?;
            store.record_dream_run(&run).map_err(|e| e.to_string())?;
            Ok(json!({"recorded": true}))
        }
        "dream.commit" => {
            let run: DreamRunRecord =
                serde_json::from_value(args.get("run").cloned().ok_or("missing run")?)
                    .map_err(|e| e.to_string())?;
            let new_memories: Vec<MemoryRecord> = serde_json::from_value(
                args.get("new_memories")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .map_err(|e| e.to_string())?;
            let updated_memories: Vec<MemoryRecord> = serde_json::from_value(
                args.get("updated_memories")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .map_err(|e| e.to_string())?;
            let sources: Vec<MemorySource> =
                serde_json::from_value(args.get("sources").cloned().unwrap_or_else(|| json!([])))
                    .map_err(|e| e.to_string())?;
            store
                .commit_dream(&run, &new_memories, &updated_memories, &sources)
                .map_err(|e| e.to_string())?;
            Ok(json!({"committed": true}))
        }
        "receipt.record" => {
            let receipt: MemoryConsumptionReceipt =
                serde_json::from_value(args.get("receipt").cloned().ok_or("missing receipt")?)
                    .map_err(|e| e.to_string())?;
            store
                .record_consumption_receipt(&receipt)
                .map_err(|e| e.to_string())?;
            Ok(json!({"recorded": true}))
        }
        "receipt.record_batch" => {
            let values = args
                .get("receipts")
                .and_then(Value::as_array)
                .ok_or("missing receipts array")?;
            if values.is_empty() || values.len() > 128 {
                return Err("receipt batch must contain 1..128 items".into());
            }
            let receipts: Vec<MemoryConsumptionReceipt> =
                serde_json::from_value(Value::Array(values.clone())).map_err(|e| e.to_string())?;
            store
                .record_consumption_receipts(&receipts)
                .map_err(|e| e.to_string())?;
            Ok(json!({"recorded": receipts.len()}))
        }
        "receipt.list" => {
            let workspace_id = as_str(&args, "workspace_id")?;
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
            let receipts = store
                .recent_consumption_receipts(workspace_id, limit)
                .map_err(|e| e.to_string())?;
            Ok(json!({"receipts": receipts}))
        }
        "lifecycle.ingest" => {
            let event = as_str(&args, "event")?.trim();
            if !matches!(
                event,
                "session.started"
                    | "turn.started"
                    | "tool.completed"
                    | "turn.completed"
                    | "session.ended"
            ) {
                return Err("unsupported lifecycle event".into());
            }
            let source_kind = args
                .get("source_kind")
                .and_then(Value::as_str)
                .unwrap_or("external")
                .trim()
                .to_ascii_lowercase();
            if source_kind.is_empty() {
                return Err("lifecycle source_kind is required".into());
            }
            let restricted_origin = match source_kind.as_str() {
                "memory" | "recall" => Some(EvidenceOrigin::Memory),
                "skill" => Some(EvidenceOrigin::Skill),
                "plugin" => Some(EvidenceOrigin::Plugin),
                "harness" => Some(EvidenceOrigin::Harness),
                "system" => Some(EvidenceOrigin::System),
                _ => None,
            };
            if restricted_origin
                .map(|origin| !assess_evidence(origin, "ENVIRONMENT_CONTEXT").learnable)
                .unwrap_or(false)
            {
                return Err("lifecycle source_kind is not eligible evidence".into());
            }
            let input_digest = as_str(&args, "input_digest")?.trim();
            if input_digest.len() != 64
                || !input_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("lifecycle input_digest must be a SHA-256 hex digest".into());
            }
            let workspace_id = as_str(&args, "workspace_id")?.trim();
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("external")
                .trim();
            let source_id = as_str(&args, "source_id")?.trim();
            let source_revision = as_str(&args, "source_revision")?.trim();
            let anonymous_source =
                hash_normalized(&format!("{workspace_id}|{source_kind}|{source_id}"));
            let anonymous_revision = hash_normalized(source_revision);
            let job = MemoryJob::new(
                format!("lifecycle:{namespace}:{event}"),
                anonymous_source,
                anonymous_revision,
                event,
                input_digest,
            )
            .map_err(|e| e.to_string())?;
            let job = store.enqueue_memory_job(&job).map_err(|e| e.to_string())?;
            Ok(json!({"job": job, "accepted": true}))
        }
        "job.enqueue" => {
            let job = MemoryJob::new(
                as_str(&args, "phase")?,
                as_str(&args, "source_id")?,
                as_str(&args, "source_revision")?,
                args.get("source_watermark")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                as_str(&args, "input_digest")?,
            )
            .map_err(|e| e.to_string())?;
            let job = store.enqueue_memory_job(&job).map_err(|e| e.to_string())?;
            Ok(json!({"job": job}))
        }
        "job.claim" => {
            let job = store
                .claim_memory_job(
                    as_str(&args, "phase")?,
                    as_str(&args, "owner")?,
                    args.get("lease_seconds")
                        .and_then(Value::as_i64)
                        .unwrap_or(120),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"job": job}))
        }
        "job.complete" => {
            let changed = store
                .complete_memory_job(
                    as_str(&args, "id")?,
                    as_str(&args, "owner")?,
                    args.get("output_digest").and_then(Value::as_str),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"changed": changed}))
        }
        "job.fail" => {
            let terminal = args
                .get("terminal")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let changed = if terminal {
                store
                    .terminal_memory_job_failure(as_str(&args, "id")?, as_str(&args, "owner")?)
                    .map_err(|e| e.to_string())?
            } else {
                store
                    .fail_memory_job(
                        as_str(&args, "id")?,
                        as_str(&args, "owner")?,
                        args.get("retry_after_seconds").and_then(Value::as_i64),
                    )
                    .map_err(|e| e.to_string())?
            };
            Ok(json!({"changed": changed, "terminal": terminal}))
        }
        "evidence.assess" => {
            let origin = match as_str(&args, "origin")?
                .trim()
                .to_ascii_uppercase()
                .as_str()
            {
                "HUMAN" => EvidenceOrigin::Human,
                "ASSISTANT" => EvidenceOrigin::Assistant,
                "SUBAGENT" => EvidenceOrigin::Subagent,
                "TOOL" => EvidenceOrigin::Tool,
                "REPOSITORY" => EvidenceOrigin::Repository,
                "MEMORY" => EvidenceOrigin::Memory,
                "SKILL" => EvidenceOrigin::Skill,
                "PLUGIN" => EvidenceOrigin::Plugin,
                "HARNESS" => EvidenceOrigin::Harness,
                "SYSTEM" => EvidenceOrigin::System,
                _ => return Err("unsupported evidence origin".into()),
            };
            let assessment = assess_evidence(origin, as_str(&args, "class")?);
            Ok(serde_json::to_value(assessment).map_err(|e| e.to_string())?)
        }
        "maintenance" => {
            let namespace = args
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("agent-cli");
            let workspace = as_str(&args, "workspace_id")?;
            let (expired, duplicates) = store
                .maintenance(namespace, workspace)
                .map_err(|e| e.to_string())?;
            Ok(json!({"expired_pruned": expired, "duplicates_consolidated": duplicates}))
        }
        other => Err(format!("unsupported memory management operation: {other}")),
    }
}
