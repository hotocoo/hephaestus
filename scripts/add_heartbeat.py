#!/usr/bin/env python3
"""Add renew_job_lease to the job store."""

path = "crates/db/src/jobs.rs"
s = open(path).read()
anchor = "    /// Fetch a dead-lettered job for inspection."
addition = (
    "    /// Renew a held lease (worker heartbeat)." + chr(10) +
    "    ///" + chr(10) +
    "    /// Only succeeds while the caller still owns a running job;" + chr(10) +
    "    /// false means ownership was lost (expired and reclaimed)." + chr(10) +
    "    pub async fn renew_job_lease(" + chr(10) +
    "        &self," + chr(10) +
    "        id: JobId," + chr(10) +
    "        owner: &str," + chr(10) +
    "        ttl_secs: i64," + chr(10) +
    "    ) -> Result<bool> {" + chr(10) +
    "        let res = sqlx::query(" + chr(10) +
    '            "UPDATE jobs SET lease_expires_at = now() + make_interval(secs => $3)"' + chr(10) +
    '             WHERE id=$1 AND lease_owner=$2 AND status=\'running\'",' + chr(10)
)
# Build the rest without escape tricks: use a plain triple-quoted block.
rest = """
        )
        .bind(id.as_uuid())
        .bind(owner)
        .bind(ttl_secs as f64)
        .execute(self.pool())
        .await
        .map_err(crate::map_sqlx)?;
        Ok(res.rows_affected() == 1)
    }

"""
addition = addition.replace(
    '             WHERE id=$1 AND lease_owner=$2 AND status='running","',
    "             WHERE id=$1 AND lease_owner=$2 AND status='running'",",
)
full = (
    "    /// Renew a held lease (worker heartbeat).
"
    "    ///
"
    "    /// Only succeeds while the caller still owns a running job;
"
    "    /// false means ownership was lost (expired and reclaimed).
"
    "    pub async fn renew_job_lease(
"
    "        &self,
"
    "        id: JobId,
"
    "        owner: &str,
"
    "        ttl_secs: i64,
"
    "    ) -> Result<bool> {
"
    "        let res = sqlx::query(
"
    '            "UPDATE jobs SET lease_expires_at = now() + make_interval(secs => $3)
'
    "             WHERE id=$1 AND lease_owner=$2 AND status='running'",
"
    "        )
"
    "        .bind(id.as_uuid())
"
    "        .bind(owner)
"
    "        .bind(ttl_secs as f64)
"
    "        .execute(self.pool())
"
    "        .await
"
    "        .map_err(crate::map_sqlx)?;
"
    "        Ok(res.rows_affected() == 1)
"
    "    }

"
)
assert anchor in s, "anchor missing"
s = s.replace(anchor, full + anchor, 1)
open(path, "w").write(s)
print("renew_job_lease added")
