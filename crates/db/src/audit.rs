//! Tamper-evident audit log with hash chaining.

use hephaestus_core::Result;
use sha2::{Digest, Sha256};

use crate::store::Db;

/// An audit entry to record.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    /// Who performed the action (principal id or system component).
    pub actor: &'a str,
    /// What happened (stable verb, e.g. "tool.execute").
    pub action: &'a str,
    /// Target entity type.
    pub target_type: &'a str,
    /// Target entity id when applicable.
    pub target_id: Option<&'a str>,
    /// Additional structured detail (must already be secret-free).
    pub detail: serde_json::Value,
}

impl Db {
    /// Record an audit entry, chained onto the previous entry's hash.
    ///
    /// The chain makes undetected tampering require rewriting the whole
    /// tail of the log; verification is available via verify_chain().
    pub async fn audit(&self, entry: &AuditEntry<'_>) -> Result<i64> {
        let prev: Option<String> =
            sqlx::query_scalar("SELECT hash FROM audit_log ORDER BY id DESC LIMIT 1")
                .fetch_optional(self.pool())
                .await
                .map_err(crate::map_sqlx)?;

        let prev_hash = prev.unwrap_or_default();
        let detail_str = entry.detail.to_string();
        let canonical = format!(
            "{prev_hash}|{}|{}|{}|{}|{}",
            entry.actor,
            entry.action,
            entry.target_type,
            entry.target_id.unwrap_or(""),
            detail_str
        );
        let hash = hex::encode(Sha256::digest(canonical.as_bytes()));

        let row = sqlx::query_scalar::<_, i64>(
            "INSERT INTO audit_log (prev_hash, hash, actor, action, target_type, target_id, detail)
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
        )
        .bind(prev_hash)
        .bind(hash)
        .bind(entry.actor)
        .bind(entry.action)
        .bind(entry.target_type)
        .bind(entry.target_id)
        .bind(entry.detail.clone())
        .fetch_one(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(row)
    }

    /// Verify the audit chain integrity end-to-end.
    ///
    /// Returns Ok(true) when every link matches; Err lists nothing -
    /// use returned boolean false plus logs for investigation.
    pub async fn verify_audit_chain(&self) -> Result<bool> {
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            serde_json::Value,
        )> = sqlx::query_as(
            "SELECT id, prev_hash, hash, actor, action, target_type, target_id, detail
                 FROM audit_log ORDER BY id ASC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(crate::map_sqlx)?;

        let mut expected_prev = String::new();
        for (id, prev_hash, hash, actor, action, target_type, target_id, detail) in rows {
            if prev_hash != expected_prev {
                tracing::warn!(audit_id = id, "audit chain broken at link");
                return Ok(false);
            }
            let canonical = format!(
                "{prev_hash}|{actor}|{action}|{target_type}|{}|{detail}",
                target_id.as_deref().unwrap_or("")
            );
            let computed = hex::encode(Sha256::digest(canonical.as_bytes()));
            if computed != hash {
                tracing::warn!(audit_id = id, "audit entry hash mismatch");
                return Ok(false);
            }
            expected_prev = hash;
        }
        Ok(true)
    }

    /// Seed helper used by tests to guarantee non-empty chain state.
    #[doc(hidden)]
    pub async fn audit_chain_head_is_empty(&self) -> Result<bool> {
        let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_log")
            .fetch_one(self.pool())
            .await
            .map_err(crate::map_sqlx)?;
        Ok(n.0 == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_db;
    use uuid::Uuid;

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_chain_records_and_verifies() {
        let db = test_db().await;
        let marker = Uuid::now_v7().simple().to_string();
        db.audit(&AuditEntry {
            actor: "tester",
            action: "test.append",
            target_type: "chain",
            target_id: Some(marker.as_str()),
            detail: serde_json::json!({"step": 1}),
        })
        .await
        .expect("append 1");
        db.audit(&AuditEntry {
            actor: "tester",
            action: "test.append",
            target_type: "chain",
            target_id: Some(marker.as_str()),
            detail: serde_json::json!({"step": 2}),
        })
        .await
        .expect("append 2");
        assert!(
            db.verify_audit_chain().await.expect("verify"),
            "freshly written chain must verify"
        );
    }
}
