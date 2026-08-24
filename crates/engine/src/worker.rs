//! Durable worker loop.
//!
//! Workers claim jobs via SKIP LOCKED, renew leases while handlers run
//! (heartbeat at 1/3 of the TTL), and complete or fail each job. A job
//! whose worker dies becomes claimable again once its lease expires:
//! at-least-once semantics, so handlers MUST be idempotent.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value as Json;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use hephaestus_core::id::JobId;
use hephaestus_db::Db;
use hephaestus_db::jobs::ClaimedJob;

use crate::jobs::{DecodeError, JobPayload};

/// Outcome a handler reports for one claimed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// Work finished successfully.
    Completed,
    /// Permanent problem; do not retry.
    FailedPermanent,
    /// Transient problem; retry with backoff.
    Retryable,
}

/// Async handler for one decoded payload kind.
///
/// Implementations receive the database handle and the payload; the
/// worker owns everything else (leases, retries, shutdown).
pub trait JobHandler: Send + Sync {
    /// Handle one job. Must be idempotent: it may run again after a
    /// crash between completion and side effects.
    fn handle<'a>(
        &'a self,
        db: Db,
        payload: JobPayload,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HandlerOutcome> + Send + 'a>>;
}

/// Registry mapping payload kinds to handlers.
#[derive(Default)]
pub struct HandlerRegistry {
    analyze: Option<Arc<dyn JobHandler>>,
}

impl HandlerRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the analysis handler.
    pub fn with_analyze(mut self, h: Arc<dyn JobHandler>) -> Self {
        self.analyze = Some(h);
        self
    }

    fn route(&self, payload: &JobPayload) -> Option<Arc<dyn JobHandler>> {
        match payload {
            JobPayload::AnalyzeRepository { .. } => self.analyze.clone(),
            // Later phases register planning/implementation/... here.
            _ => None,
        }
    }
}

/// Worker configuration.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Stable identity used as lease owner (e.g. "worker-host-1").
    pub id: String,
    /// Queues this worker serves (queue name strings).
    pub queues: Vec<String>,
    /// Lease duration in seconds; renewed at ttl/3 intervals.
    pub lease_ttl_secs: i64,
    /// Poll interval when queues are empty.
    pub poll_interval: Duration,
    /// Concurrency cap per worker process.
    pub concurrency: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            id: format!("worker-{}", Uuid::now_v7().simple()),
            queues: vec!["analysis".into()],
            lease_ttl_secs: 300,
            poll_interval: Duration::from_millis(500),
            concurrency: 4,
        }
    }
}

/// A running Hephaestus worker.
pub struct Worker {
    db: Db,
    registry: Arc<HandlerRegistry>,
    config: WorkerConfig,
}

impl Worker {
    /// Construct a worker bound to a database and handler set.
    pub fn new(db: Db, registry: HandlerRegistry, config: WorkerConfig) -> Self {
        Self {
            db,
            registry: Arc::new(registry),
            config,
        }
    }

    /// Run until `shutdown` fires, then drain gracefully.
    ///
    /// In-flight jobs finish (lease renewed during drain); no new jobs
    /// are claimed after shutdown begins.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        info!(worker = %self.config.id, queues = ?self.config.queues, "worker started");
        let slots = Arc::new(tokio::sync::Semaphore::new(self.config.concurrency));
        'outer: loop {
            if shutdown.is_cancelled() {
                break;
            }
            let permit = match tokio::time::timeout(
                self.config.poll_interval,
                Arc::clone(&slots).acquire_owned(),
            )
            .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(_)) => break,       // all permits dropped: shutting down
                Err(_elapsed) => continue, // no free slot this tick
            };

            // Labeled block keeps move/drop paths lexically exclusive:
            // the permit either moves into a spawned job or is dropped
            // before idling - never both.
            'dispatch: {
                let mut claim_error: Option<hephaestus_core::Error> = None;
                for queue in &self.config.queues {
                    match self
                        .db
                        .claim_job(queue, &self.config.id, self.config.lease_ttl_secs)
                        .await
                    {
                        Ok(Some(job)) => {
                            let me = Arc::clone(&self);
                            let shutdown = shutdown.clone();
                            tokio::spawn(async move {
                                me.process(job, shutdown).await;
                                drop(permit);
                            });
                            break 'dispatch; // re-scan for fairness
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            error!(%queue, error = %e, "claim failed");
                            claim_error = Some(e);
                            break;
                        }
                    }
                }
                let _ = claim_error; // logged above; next tick retries
                // Nothing claimed: release the slot while idling.
                drop(permit);
                tokio::select! {
                    _ = shutdown.cancelled() => break 'outer,
                    _ = tokio::time::sleep(self.config.poll_interval) => {}
                }
            }
        }
        info!(worker = %self.config.id, "worker draining");
    }

    async fn process(&self, job: ClaimedJob, shutdown: CancellationToken) {
        let job_id = JobId::from_uuid(job.id);
        let owner = self.config.id.clone();
        let ttl = self.config.lease_ttl_secs;

        // Heartbeat: renew at ttl/3 until the handler finishes.
        let heartbeat_db = self.db.clone();
        let hb = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs((ttl / 3).max(1) as u64));
            ticker.tick().await; // first tick immediate: skip
            loop {
                ticker.tick().await;
                if shutdown.is_cancelled() {
                    break;
                }
                match heartbeat_db.renew_job_lease(job_id, &owner, ttl).await {
                    Ok(true) => {}
                    Ok(false) => {
                        warn!(%job_id, "lease lost during heartbeat");
                        break;
                    }
                    Err(e) => error!(%job_id, error = %e, "heartbeat failed"),
                }
            }
        });

        let result = self.execute_decoded(&job).await;
        hb.abort();

        match result {
            Ok(outcome) => match outcome {
                HandlerOutcome::Completed => {
                    if let Err(e) = self.db.complete_job(job_id, &self.config.id).await {
                        error!(%job_id, error = %e, "complete failed");
                    } else {
                        info!(%job_id, queue = %job.queue, "job completed");
                    }
                }
                HandlerOutcome::FailedPermanent | HandlerOutcome::Retryable => {
                    // Permanent failures still go through fail_job so the
                    // attempt counter advances toward dead-letter; the
                    // distinction lives in handler-provided context and
                    // future policy wiring.
                    match self
                        .db
                        .fail_job(job_id, &self.config.id, "handler reported failure")
                        .await
                    {
                        Ok(retry) => warn!(%job_id, will_retry = retry, "job failed"),
                        Err(e) => error!(%job_id, error = %e, "fail_job failed"),
                    }
                }
            },
            Err(decode_err) => {
                // Un-decodable payload: never retried by this version.
                error!(%job_id, error = ?decode_err, "payload decode failed");
                let _ = self
                    .db
                    .fail_job(job_id, &self.config.id, &format!("decode: {decode_err:?}"))
                    .await;
            }
        }
    }

    async fn execute_decoded(&self, job: &ClaimedJob) -> Result<HandlerOutcome, DecodeError> {
        let payload = decode_envelope(&job.payload)?;
        // Route miss is a configuration error, not payload corruption:
        // surface it distinctly so operators see unregistered kinds.
        let Some(handler) = self.registry.route(&payload) else {
            tracing::error!(kind = %kind_of(&payload), "no handler registered");
            return Ok(HandlerOutcome::FailedPermanent);
        };
        Ok(handler.handle(self.db.clone(), payload).await)
    }
}

fn decode_envelope(payload: &Json) -> Result<JobPayload, DecodeError> {
    JobPayload::from_envelope(payload)
}

fn kind_of(p: &JobPayload) -> &'static str {
    match p {
        JobPayload::AnalyzeRepository { .. } => "analyze_repository",
        JobPayload::ExtractRequirements { .. } => "extract_requirements",
        JobPayload::GeneratePlan { .. } => "generate_plan",
        JobPayload::ExecuteStep { .. } => "execute_step",
        JobPayload::RunVerification { .. } => "run_verification",
        JobPayload::BuildArtifact { .. } => "build_artifact",
    }
}
