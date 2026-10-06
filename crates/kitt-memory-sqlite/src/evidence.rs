use kitt_memory_core::{MemoryConsumptionReceipt, MemoryError, MemoryJob, Result, now_epoch};
use rusqlite::{OptionalExtension, params};

use crate::{SqliteMemoryStore, storage};

impl SqliteMemoryStore {
    pub fn record_consumption_receipt(&self, receipt: &MemoryConsumptionReceipt) -> Result<()> {
        self.record_consumption_receipts(std::slice::from_ref(receipt))
    }

    pub fn record_consumption_receipts(&self, receipts: &[MemoryConsumptionReceipt]) -> Result<()> {
        if receipts.is_empty() || receipts.len() > 128 {
            return Err(MemoryError::Invalid(
                "receipt batch must contain 1..128 items".into(),
            ));
        }
        for receipt in receipts {
            receipt.validate()?;
        }
        self.flush_recall_traces()?;
        let mut writer = self.writer_conn()?;
        let conn = writer.transaction().map_err(storage)?;
        for receipt in receipts {
            conn.execute(
                "INSERT INTO memory_consumption_receipts(
                recall_trace_id,memory_id,consumer,purpose,presented,referenced,
                used_for_action,outcome,turn_id,consumed_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(recall_trace_id,memory_id,consumer,purpose,turn_id)
             DO UPDATE SET
                presented=MAX(presented,excluded.presented),
                referenced=MAX(referenced,excluded.referenced),
                used_for_action=MAX(used_for_action,excluded.used_for_action),
                outcome=CASE WHEN excluded.outcome<>'' THEN excluded.outcome ELSE outcome END,
                consumed_at=MAX(consumed_at,excluded.consumed_at)",
                params![
                    receipt.recall_trace_id,
                    receipt.memory_id,
                    receipt.consumer,
                    receipt.purpose,
                    receipt.presented as i64,
                    receipt.referenced as i64,
                    receipt.used_for_action as i64,
                    receipt.outcome,
                    receipt.turn_id,
                    receipt.consumed_at,
                ],
            )
            .map_err(storage)?;
            if receipt.referenced || receipt.used_for_action {
                conn.execute(
                    "UPDATE memories
                 SET last_accessed_at=?1,access_count=access_count+1
                 WHERE id=?2",
                    params![receipt.consumed_at.max(now_epoch()), receipt.memory_id],
                )
                .map_err(storage)?;
            }
        }
        conn.commit().map_err(storage)?;
        Ok(())
    }

    pub fn recent_consumption_receipts(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryConsumptionReceipt>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT r.recall_trace_id,r.memory_id,r.consumer,r.purpose,
                        r.presented,r.referenced,r.used_for_action,r.outcome,
                        r.turn_id,r.consumed_at
                 FROM memory_consumption_receipts r
                 JOIN recall_traces t ON t.id=r.recall_trace_id
                 WHERE t.workspace_id=?1
                 ORDER BY r.consumed_at DESC,r.rowid DESC LIMIT ?2",
            )?;
            stmt.query_map(params![workspace_id, limit.min(1024) as i64], |row| {
                Ok(MemoryConsumptionReceipt {
                    recall_trace_id: row.get(0)?,
                    memory_id: row.get(1)?,
                    consumer: row.get(2)?,
                    purpose: row.get(3)?,
                    presented: row.get::<_, i64>(4)? != 0,
                    referenced: row.get::<_, i64>(5)? != 0,
                    used_for_action: row.get::<_, i64>(6)? != 0,
                    outcome: row.get(7)?,
                    turn_id: row.get(8)?,
                    consumed_at: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
        })
    }

    pub fn enqueue_memory_job(&self, job: &MemoryJob) -> Result<MemoryJob> {
        job.validate()?;
        let conn = self.writer_conn()?;
        conn.execute(
            "INSERT INTO memory_jobs(
                id,phase,source_id,source_revision,source_watermark,status,
                lease_owner,lease_until,attempt,next_retry_at,input_digest,
                output_digest,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,'PENDING',NULL,NULL,0,NULL,?6,NULL,?7,?8)
             ON CONFLICT(phase,source_id,source_revision,input_digest) DO NOTHING",
            params![
                job.id,
                job.phase,
                job.source_id,
                job.source_revision,
                job.source_watermark,
                job.input_digest,
                job.created_at,
                job.updated_at,
            ],
        )
        .map_err(storage)?;
        conn.query_row(
            "SELECT id,phase,source_id,source_revision,source_watermark,status,
                    lease_owner,lease_until,attempt,next_retry_at,input_digest,
                    output_digest,created_at,updated_at
             FROM memory_jobs
             WHERE phase=?1 AND source_id=?2 AND source_revision=?3 AND input_digest=?4",
            params![
                job.phase,
                job.source_id,
                job.source_revision,
                job.input_digest
            ],
            map_job,
        )
        .map_err(storage)
    }

    pub fn claim_memory_job(
        &self,
        phase: &str,
        owner: &str,
        lease_seconds: i64,
    ) -> Result<Option<MemoryJob>> {
        let owner = owner.trim();
        let phase = phase.trim();
        if owner.is_empty() || phase.is_empty() {
            return Err(MemoryError::Invalid(
                "job phase and owner are required".into(),
            ));
        }
        let now = now_epoch();
        let lease_until = now.saturating_add(lease_seconds.clamp(5, 3600));
        let conn = self.writer_conn()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM memory_jobs
                 WHERE phase=?1
                   AND (
                     status='PENDING'
                     OR (status='RETRY' AND COALESCE(next_retry_at,0)<=?2)
                     OR (status='RUNNING' AND COALESCE(lease_until,0)<=?2)
                   )
                 ORDER BY created_at ASC,id ASC LIMIT 1",
                params![phase, now],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        let Some(id) = id else {
            return Ok(None);
        };
        let changed = conn
            .execute(
                "UPDATE memory_jobs
                 SET status='RUNNING',lease_owner=?1,lease_until=?2,
                     attempt=attempt+1,updated_at=?3
                 WHERE id=?4
                   AND (
                     status='PENDING'
                     OR (status='RETRY' AND COALESCE(next_retry_at,0)<=?3)
                     OR (status='RUNNING' AND COALESCE(lease_until,0)<=?3)
                   )",
                params![owner, lease_until, now, id],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT id,phase,source_id,source_revision,source_watermark,status,
                    lease_owner,lease_until,attempt,next_retry_at,input_digest,
                    output_digest,created_at,updated_at
             FROM memory_jobs WHERE id=?1",
            [&id],
            map_job,
        )
        .optional()
        .map_err(storage)
    }

    pub fn complete_memory_job(
        &self,
        id: &str,
        owner: &str,
        output_digest: Option<&str>,
    ) -> Result<bool> {
        let conn = self.writer_conn()?;
        let changed = conn
            .execute(
                "UPDATE memory_jobs
                 SET status='SUCCEEDED',lease_owner=NULL,lease_until=NULL,
                     next_retry_at=NULL,output_digest=?1,updated_at=?2
                 WHERE id=?3 AND status='RUNNING' AND lease_owner=?4",
                params![output_digest, now_epoch(), id, owner],
            )
            .map_err(storage)?;
        Ok(changed == 1)
    }

    pub fn fail_memory_job(
        &self,
        id: &str,
        owner: &str,
        retry_after_seconds: Option<i64>,
    ) -> Result<bool> {
        let now = now_epoch();
        let conn = self.writer_conn()?;
        let attempt: Option<i64> = conn
            .query_row(
                "SELECT attempt FROM memory_jobs
                 WHERE id=?1 AND status='RUNNING' AND lease_owner=?2",
                params![id, owner],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        let Some(attempt) = attempt else {
            return Ok(false);
        };
        let bounded_attempt = attempt.clamp(1, 31) as u32;
        let delay = retry_after_seconds.unwrap_or_else(|| {
            let exponent = bounded_attempt.saturating_sub(1).min(10);
            5_i64.saturating_mul(1_i64 << exponent).min(3600)
        });
        let retry_at = now.saturating_add(delay.clamp(1, 3600));
        let changed = conn
            .execute(
                "UPDATE memory_jobs
                 SET status='RETRY',lease_owner=NULL,lease_until=NULL,
                     next_retry_at=?1,updated_at=?2
                 WHERE id=?3 AND status='RUNNING' AND lease_owner=?4",
                params![retry_at, now, id, owner],
            )
            .map_err(storage)?;
        Ok(changed == 1)
    }

    pub fn terminal_memory_job_failure(&self, id: &str, owner: &str) -> Result<bool> {
        let conn = self.writer_conn()?;
        let changed = conn
            .execute(
                "UPDATE memory_jobs
                 SET status='FAILED',lease_owner=NULL,lease_until=NULL,
                     next_retry_at=NULL,updated_at=?1
                 WHERE id=?2 AND status='RUNNING' AND lease_owner=?3",
                params![now_epoch(), id, owner],
            )
            .map_err(storage)?;
        Ok(changed == 1)
    }
}

fn map_job(row: &rusqlite::Row<'_>) -> std::result::Result<MemoryJob, rusqlite::Error> {
    let attempt: i64 = row.get(8)?;
    if attempt < 0 || attempt > u32::MAX as i64 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(8, attempt));
    }
    Ok(MemoryJob {
        id: row.get(0)?,
        phase: row.get(1)?,
        source_id: row.get(2)?,
        source_revision: row.get(3)?,
        source_watermark: row.get(4)?,
        status: row.get(5)?,
        lease_owner: row.get(6)?,
        lease_until: row.get(7)?,
        attempt: attempt as u32,
        next_retry_at: row.get(9)?,
        input_digest: row.get(10)?,
        output_digest: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}
