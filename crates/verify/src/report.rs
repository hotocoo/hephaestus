//! Execution of plans and the resulting report.

use serde::Serialize;
use std::time::{Duration, Instant};

use hephaestus_tools::SandboxedShell;
use hephaestus_tools::capabilities::CapabilitySet;

use crate::plan::VerificationPlan;

/// Result of one layer.
#[derive(Debug, Clone, Serialize)]
pub struct LayerResult {
    /// Canonical layer name.
    pub layer: &'static str,
    /// Exit code (None when timed out).
    pub exit_code: Option<i32>,
    /// Pass = exit code 0 and not timed out.
    pub passed: bool,
    /// Wall-clock duration.
    #[serde(with = "duration_millis")]
    pub duration: Duration,
    /// First 4 KiB of combined output for evidence display.
    pub output_excerpt: String,
}

mod duration_millis {
    // Serialize-only for now; the evidence API that serves stored
    // reports adds the deserializer with its own tests.
    use serde::Serializer;
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }
}

/// Aggregated outcome of a plan run.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    /// Per-layer results in execution order.
    pub results: Vec<LayerResult>,
}

impl VerificationReport {
    /// True only when EVERY required layer passed.
    ///
    /// A missing layer cannot pass by definition - the plan defines
    /// what is required, so an empty report fails.
    pub fn all_passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.passed)
    }
}

/// Execute a plan against a workspace using governed shell access.
///
/// Stops at the first failing layer (fail-fast) unless `continue_on_failure`
/// is set; either way the report records exactly what ran.
pub fn run_plan(
    plan: &VerificationPlan,
    caps: &CapabilitySet,
    continue_on_failure: bool,
) -> VerificationReport {
    let mut results = Vec::new();
    for spec in plan.layers() {
        let shell = SandboxedShell::new(caps).with_timeout(Duration::from_secs(900));
        let started = Instant::now();

        let arg_refs: Vec<&str> = spec.args.iter().map(String::as_str).collect();
        let outcome = match shell.execute(&spec.program, &arg_refs) {
            Ok(o) => o,
            // Forbidden here means misconfiguration (capability or
            // allowlist): record as failed layer with the reason rather
            // than panicking - verification must be observable.
            Err(e) => {
                return finish(
                    results,
                    Some(LayerResult {
                        layer: spec.layer.as_str(),
                        exit_code: None,
                        passed: false,
                        duration: started.elapsed(),
                        output_excerpt: format!("execution refused: {e}"),
                    }),
                );
            }
        };

        let passed = !outcome.timed_out && outcome.exit_code == Some(0);
        let mut excerpt = format!("{}{}", outcome.stdout, outcome.stderr);
        if excerpt.len() > 4096 {
            excerpt.truncate(4096);
            excerpt.push_str("...[truncated]");
        }
        results.push(LayerResult {
            layer: spec.layer.as_str(),
            exit_code: outcome.exit_code,
            passed,
            duration: started.elapsed(),
            output_excerpt: excerpt,
        });

        if !passed && !continue_on_failure {
            break;
        }
    }
    finish(results, None)
}

fn finish(mut results: Vec<LayerResult>, extra: Option<LayerResult>) -> VerificationReport {
    if let Some(e) = extra {
        results.push(e);
    }
    VerificationReport { results }
}
