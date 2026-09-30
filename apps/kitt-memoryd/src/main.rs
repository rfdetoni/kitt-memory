use kitt_memory_core::{
    DreamRunRecord, KnowledgeRelation, KnowledgeStore, MemoryConsumptionReceipt, MemoryJob,
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryStatus, MemoryStore, NewConcept,
    NewCorrection, NewMemory, RecallQuery, RecallTrace, SemanticMemoryStore, Sensitivity,
    hash_normalized, now_epoch,
};
use kitt_memory_sqlite::SqliteMemoryStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Instant,
};
use uuid::Uuid;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const DEFAULT_ADDR: &str = "127.0.0.1:41829";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u16,
    id: String,
    kind: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    token: String,
    envelope: Envelope,
}

#[derive(Debug, Serialize)]
struct ResponseEnvelope {
    version: u16,
    id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    payload: Value,
}

fn response(kind: &str, request_id: &str, payload: Value) -> ResponseEnvelope {
    ResponseEnvelope {
        version: 1,
        id: Uuid::new_v4().to_string(),
        kind: kind.to_string(),
        correlation_id: Some(request_id.to_string()),
        payload,
    }
}

fn error(request_id: Option<&str>, code: &str, message: impl Into<String>) -> ResponseEnvelope {
    ResponseEnvelope {
        version: 1,
        id: Uuid::new_v4().to_string(),
        kind: "system.error".into(),
        correlation_id: request_id.map(str::to_string),
        payload: json!({"code": code, "message": message.into()}),
    }
}

fn config_root() -> PathBuf {
    if let Some(value) = env::var_os("KITT_MEMORY_CONFIG_DIR") {
        return PathBuf::from(value);
    }
    #[cfg(target_os = "windows")]
    {
        return PathBuf::from(env::var_os("APPDATA").unwrap_or_else(|| ".".into()))
            .join("kitt")
            .join("memory");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join("Library/Application Support/kitt/memory");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".config")
            });
        base.join("kitt/memory")
    }
}

fn data_root() -> PathBuf {
    if let Some(value) = env::var_os("KITT_MEMORY_DATA_DIR") {
        return PathBuf::from(value);
    }
    #[cfg(target_os = "windows")]
    {
        return PathBuf::from(env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
            .join("kitt")
            .join("memory");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join("Library/Application Support/kitt/memory");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into()))
                    .join(".local/share")
            });
        base.join("kitt/memory")
    }
}

fn ensure_token(path: &Path) -> std::io::Result<String> {
    if let Ok(value) = fs::read_to_string(path) {
        let value = value.trim().to_string();
        if value.len() >= 48 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Ok(value);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(token.as_bytes())?;
    Ok(token)
}

fn parse_kind(raw: &str) -> Result<MemoryKind, String> {
    MemoryKind::from_db(&raw.trim().replace('-', "_").to_ascii_uppercase())
        .map_err(|e| e.to_string())
}

fn parse_status(raw: &str) -> Result<MemoryStatus, String> {
    MemoryStatus::from_db(&raw.trim().to_ascii_uppercase()).map_err(|e| e.to_string())
}

fn parse_scope(raw: &str) -> Result<MemoryScope, String> {
    MemoryScope::from_db(&raw.trim().to_ascii_lowercase()).map_err(|e| e.to_string())
}

fn parse_sensitivity(raw: &str) -> Result<Sensitivity, String> {
    Sensitivity::from_db(&raw.trim().to_ascii_lowercase()).map_err(|e| e.to_string())
}

fn as_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {key}"))
}

fn manage(store: &SqliteMemoryStore, payload: &Value) -> Result<Value, String> {
    let operation = as_str(payload, "operation")?;
    let args = payload
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match operation {
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
            if source_kind.is_empty()
                || source_kind.contains("memory")
                || source_kind.contains("recall")
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
            let changed = store
                .fail_memory_job(
                    as_str(&args, "id")?,
                    as_str(&args, "owner")?,
                    args.get("retry_after_seconds").and_then(Value::as_i64),
                )
                .map_err(|e| e.to_string())?;
            Ok(json!({"changed": changed}))
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

fn handle(store: &SqliteMemoryStore, frame: Frame, token: &str) -> ResponseEnvelope {
    if frame.token != token {
        return error(
            Some(&frame.envelope.id),
            "unauthorized",
            "invalid memory service token",
        );
    }
    if frame.envelope.version != 1 || frame.envelope.id.trim().is_empty() {
        return error(
            Some(&frame.envelope.id),
            "invalid_request",
            "invalid protocol envelope",
        );
    }
    let id = frame.envelope.id.as_str();
    match frame.envelope.kind.as_str() {
        "system.ping.request" => response(
            "system.ping.response",
            id,
            json!({"service":"kitt-memoryd","ok":true}),
        ),
        "memory.remember.request" => {
            let p = &frame.envelope.payload;
            let result = (|| -> Result<MemoryRecord, String> {
                let memory = NewMemory {
                    namespace: as_str(p, "namespace")?.to_string(),
                    workspace_id: as_str(p, "workspace_id")?.to_string(),
                    kind: parse_kind(as_str(p, "kind")?)?,
                    content: as_str(p, "content")?.to_string(),
                    sensitivity: parse_sensitivity(
                        p.get("sensitivity")
                            .and_then(Value::as_str)
                            .unwrap_or("private"),
                    )?,
                    scope: parse_scope(
                        p.get("scope")
                            .and_then(Value::as_str)
                            .unwrap_or("workspace"),
                    )?,
                    scope_key: p
                        .get("scope_key")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    importance: p.get("importance").and_then(Value::as_f64).unwrap_or(0.8) as f32,
                    confidence: p.get("confidence").and_then(Value::as_f64).unwrap_or(1.0) as f32,
                    pinned: p.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                    ttl_seconds: p.get("ttl_seconds").and_then(Value::as_u64),
                    metadata_json: "{}".into(),
                };
                store.remember(memory).map_err(|e| e.to_string())
            })();
            match result {
                Ok(record) => response(
                    "memory.remember.response",
                    id,
                    json!({"id":record.id,"record":record}),
                ),
                Err(message) => error(Some(id), "memory_error", message),
            }
        }
        "memory.recall.request" => {
            let p = &frame.envelope.payload;
            let started = Instant::now();
            let query = RecallQuery {
                namespace: p
                    .get("namespace")
                    .and_then(Value::as_str)
                    .unwrap_or("agent-cli")
                    .to_string(),
                workspace_id: p
                    .get("workspace_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                scope_key: p
                    .get("scope_key")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: p
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                limit: p.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize,
                as_of: p.get("as_of").and_then(Value::as_i64),
                allow_private: p
                    .get("allow_private")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                allow_secret: p
                    .get("allow_secret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            if query.workspace_id.trim().is_empty() {
                error(Some(id), "memory_error", "missing workspace_id")
            } else {
                match store.recall(&query) {
                    Ok(records) => {
                        let trace_id = format!("recall_{}", Uuid::new_v4().simple());
                        let selected = records
                            .iter()
                            .map(|record| record.id.clone())
                            .collect::<Vec<_>>();
                        let selected_json =
                            serde_json::to_string(&selected).unwrap_or_else(|_| "[]".into());
                        let context_hash = kitt_memory_core::hash_normalized(&format!(
                            "{}|{}|{}",
                            query.namespace, query.workspace_id, selected_json
                        ));
                        let trace = RecallTrace {
                            id: trace_id.clone(),
                            namespace: query.namespace.clone(),
                            workspace_id: query.workspace_id.clone(),
                            query: query.text.clone(),
                            planned_scopes_json: serde_json::to_string(&json!({
                                "scope_key": query.scope_key,
                                "as_of": query.as_of,
                                "allow_private": query.allow_private,
                                "allow_secret": query.allow_secret
                            }))
                            .unwrap_or_else(|_| "{}".into()),
                            candidates_json: selected_json.clone(),
                            selected_json,
                            token_cost: records
                                .iter()
                                .map(|record| record.content.len().div_ceil(4) as u64)
                                .sum(),
                            semantic_fallback: false,
                            elapsed_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                            context_hash,
                            created_at: now_epoch(),
                        };
                        if let Err(err) = store.record_recall_trace(&trace) {
                            error(Some(id), "memory_error", err.to_string())
                        } else {
                            response(
                                "memory.recall.response",
                                id,
                                json!({
                                    "records": records,
                                    "recall_trace_id": trace_id
                                }),
                            )
                        }
                    }
                    Err(message) => error(Some(id), "memory_error", message.to_string()),
                }
            }
        }
        "memory.forget.request" => match store.forget(
            frame
                .envelope
                .payload
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ) {
            Ok(deleted) => response("memory.forget.response", id, json!({"deleted":deleted})),
            Err(e) => error(Some(id), "memory_error", e.to_string()),
        },
        "memory.manage.request" => match manage(store, &frame.envelope.payload) {
            Ok(value) => response("memory.manage.response", id, value),
            Err(message) => error(Some(id), "memory_error", message),
        },
        _ => error(
            Some(id),
            "unsupported_kind",
            format!("unsupported kind {}", frame.envelope.kind),
        ),
    }
}

fn serve_connection(mut stream: TcpStream, store: Arc<SqliteMemoryStore>, token: Arc<String>) {
    let clone = match stream.try_clone() {
        Ok(value) => value,
        Err(_) => return,
    };
    let mut reader = BufReader::new(clone);
    let mut line = Vec::new();
    match reader.read_until(b'\n', &mut line) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    if line.len() > MAX_FRAME_BYTES + 1 {
        let _ = writeln!(
            stream,
            "{}",
            serde_json::to_string(&error(
                None,
                "frame_too_large",
                "request exceeds frame limit"
            ))
            .unwrap()
        );
        return;
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    let reply = match serde_json::from_slice::<Frame>(&line) {
        Ok(frame) => handle(&store, frame, &token),
        Err(e) => error(None, "invalid_json", e.to_string()),
    };
    if let Ok(encoded) = serde_json::to_string(&reply) {
        let _ = stream.write_all(encoded.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("kitt-memoryd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "kitt-memoryd {}\n\nStandalone loopback memory authority for K.I.T.T.\nEnvironment: KITT_MEMORY_ADDR, KITT_MEMORY_TOKEN_PATH, KITT_MEMORY_DB",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let addr = env::var("KITT_MEMORY_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    if !addr.starts_with("127.0.0.1:")
        && !addr.starts_with("[::1]:")
        && !addr.starts_with("localhost:")
    {
        return Err("KITT_MEMORY_ADDR must be loopback".into());
    }
    let config = config_root();
    let data = data_root();
    fs::create_dir_all(&config)?;
    fs::create_dir_all(&data)?;
    let token_path = env::var_os("KITT_MEMORY_TOKEN_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.join("auth.token"));
    let db_path = env::var_os("KITT_MEMORY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("memory.sqlite3"));
    let token = Arc::new(ensure_token(&token_path)?);
    let store = Arc::new(SqliteMemoryStore::open(db_path)?);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("kitt-memoryd listening on {addr}");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let store = Arc::clone(&store);
                let token = Arc::clone(&token);
                thread::spawn(move || serve_connection(stream, store, token));
            }
            Err(error) => eprintln!("kitt-memoryd accept error: {error}"),
        }
    }
    Ok(())
}
