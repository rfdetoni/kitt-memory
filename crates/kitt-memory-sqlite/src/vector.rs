//! Optional, locally indexed cosine retrieval. SQLite remains authoritative.
//! Embeddings are derived data; stale content hashes and inaccessible scopes never match.
use super::*;

const MAX_DIMS: usize = 2048;
const MIN_DIMS: usize = 2;
const VECTOR_SCAN_CAP: i64 = 4096;

fn normalized(values: &[f32]) -> Result<Vec<f32>> {
    if !(MIN_DIMS..=MAX_DIMS).contains(&values.len())
        || values.iter().any(|v| !v.is_finite())
    {
        return Err(MemoryError::Invalid("embedding must contain 2..2048 finite floats".into()));
    }
    let length = values.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>().sqrt();
    if length <= 1e-12 || !length.is_finite() {
        return Err(MemoryError::Invalid("zero/invalid embedding norm".into()));
    }
    Ok(values.iter().map(|v| (*v as f64 / length) as f32).collect())
}

/// Eight fixed random-hyperplane signs. This is an approximate candidate index,
/// not a substitute for a high-recall ANN engine on very large collections.
fn bucket(values: &[f32]) -> i64 {
    let mut bits = 0_i64;
    for bit in 0..8 {
        let mut sum = 0_f64;
        for (dimension, value) in values.iter().enumerate() {
            let mut x = (dimension as u64 + 1)
                .wrapping_mul(0x9e3779b97f4a7c15)
                ^ ((bit as u64 + 1).wrapping_mul(0xbf58476d1ce4e5b9));
            x ^= x >> 30;
            x = x.wrapping_mul(0xbf58476d1ce4e5b9);
            x ^= x >> 27;
            x = x.wrapping_mul(0x94d049bb133111eb);
            x ^= x >> 31;
            sum += *value as f64 * if x & 1 == 0 { 1.0 } else { -1.0 };
        }
        if sum >= 0.0 {
            bits |= 1 << bit;
        }
    }
    bits
}

fn vector_blob(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn dot(query: &[f32], raw: &[u8]) -> Option<f32> {
    if raw.len() != query.len() * 4 {
        return None;
    }
    let sum = raw.chunks_exact(4).zip(query.iter()).map(|(bytes, value)| {
        f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) * value
    }).sum::<f32>();
    sum.is_finite().then_some(sum.clamp(-1.0, 1.0))
}

impl SqliteMemoryStore {
    /// Upsert a precomputed embedding only if its memory is currently active.
    /// Both embedding/model and memory content hash must agree at query time.
    pub fn upsert_embedding(&self, memory_id: &str, model: &str, embedding: &[f32]) -> Result<bool> {
        if model.is_empty() || model.len() > 128 || !model.is_ascii()
            || model.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(MemoryError::Invalid("invalid embedding model".into()));
        }
        let embedding = normalized(embedding)?;
        let mut conn = self.writer_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(storage)?;
        let inserted = tx.execute(
            "INSERT INTO memory_embeddings(memory_id,model,dimensions,content_hash,bucket,vector,updated_at)
             SELECT m.id,?2,?3,m.content_hash,?4,?5,unixepoch()
             FROM memories m WHERE m.id=?1 AND m.status='ACTIVE' AND m.sensitivity<>'ephemeral'
             ON CONFLICT(memory_id,model) DO UPDATE SET
               dimensions=excluded.dimensions,content_hash=excluded.content_hash,
               bucket=excluded.bucket,vector=excluded.vector,updated_at=excluded.updated_at",
            params![memory_id, model, embedding.len() as i64, bucket(&embedding), vector_blob(&embedding)],
        ).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(inserted > 0)
    }

    /// Remove all derived vectors for a model, so its index can be regenerated.
    pub fn clear_embeddings(&self, model: &str) -> Result<usize> {
        if model.is_empty() || model.len() > 128 {
            return Err(MemoryError::Invalid("invalid embedding model".into()));
        }
        self.writer_conn()?
            .execute("DELETE FROM memory_embeddings WHERE model=?1", [model])
            .map_err(storage)
    }

    fn vector_candidates(
        &self, query: &RecallQuery, model: &str, embedding: &[f32], limit: usize,
    ) -> Result<Vec<(MemoryRecord, f32)>> {
        let embedding = normalized(embedding)?;
        let center = bucket(&embedding);
        let buckets = [
            center, center ^ 1, center ^ 2, center ^ 4, center ^ 8,
            center ^ 16, center ^ 32, center ^ 64, center ^ 128,
        ];
        let at = query.as_of.unwrap_or_else(now_epoch);
        let scope_key = query.scope_key.as_deref().unwrap_or("");
        let mut results = self.with_conn(|conn| {
            let sql = format!(
                "SELECT {MEMORY_COLUMNS},v.vector FROM memory_embeddings v
                 JOIN memories m ON m.id=v.memory_id
                 WHERE v.model=?1 AND v.dimensions=?2 AND v.bucket IN
                   (?3,?4,?5,?6,?7,?8,?9,?10,?11)
                   AND v.content_hash=m.content_hash AND m.namespace=?12
                   AND (m.scope='global' OR (m.workspace_id=?13 AND m.scope='workspace')
                     OR (m.workspace_id=?13 AND m.scope='conversation' AND m.scope_key=?14))
                   AND m.status='ACTIVE'
                   AND (m.valid_from IS NULL OR m.valid_from<=?15)
                   AND (m.valid_until IS NULL OR m.valid_until>?15)
                   AND m.sensitivity<>'ephemeral'
                   AND (m.sensitivity<>'private' OR ?16=1)
                   AND (m.sensitivity<>'secret' OR ?17=1)
                 LIMIT ?18"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![
                model, embedding.len() as i64,
                buckets[0], buckets[1], buckets[2], buckets[3], buckets[4],
                buckets[5], buckets[6], buckets[7], buckets[8],
                query.namespace, query.workspace_id, scope_key, at,
                query.allow_private as i64, query.allow_secret as i64, VECTOR_SCAN_CAP,
            ], |row| Ok((map_memory_row(row)?, row.get::<_, Vec<u8>>(24)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
        })?;
        let mut hits = Vec::new();
        for (record, raw) in results.drain(..) {
            if let Some(score) = dot(&embedding, &raw) {
                // Reject orthogonal/unrelated candidates; lexical fallback stays available.
                if score >= 0.30 {
                    hits.push((record, score));
                }
            }
        }
        hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        hits.truncate(limit.min(128));
        Ok(hits)
    }

    /// Hybrid lexical+vector reciprocal-rank fusion (RRF), deduplicated by memory ID.
    /// No query vector means the existing lexical/retention search is unchanged.
    pub fn search_hybrid(
        &self, query: &RecallQuery, model: &str, embedding: &[f32],
    ) -> Result<Vec<MemoryRecord>> {
        if embedding.is_empty() || query.text.trim().is_empty() || query.limit == 0 {
            return self.recall(query);
        }
        let _ = normalized(embedding)?;
        let limit = query.limit.min(50);
        let mut expanded = query.clone();
        expanded.limit = limit.saturating_mul(2).min(50);
        let lexical = self.recall(&expanded)?;
        let semantic = self.vector_candidates(query, model, embedding, (limit * 4).min(128))?;
        if semantic.is_empty() {
            return Ok(lexical.into_iter().take(limit).collect());
        }
        let mut ranks: HashMap<String, (f32, MemoryRecord)> = HashMap::new();
        for (index, record) in lexical.into_iter().enumerate() {
            let score = 1.0 / (60.0 + index as f32 + 1.0);
            ranks.insert(record.id.clone(), (score, record));
        }
        for (index, (record, _score)) in semantic.into_iter().enumerate() {
            let score = 1.2 / (60.0 + index as f32 + 1.0);
            ranks.entry(record.id.clone())
                .and_modify(|existing| existing.0 += score)
                .or_insert((score, record));
        }
        let mut ranked = ranks.into_values().collect::<Vec<_>>();
        ranked.sort_by(|a,b| b.0.total_cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(ranked.into_iter().take(limit).map(|(_,record)| record).collect())
    }
}
