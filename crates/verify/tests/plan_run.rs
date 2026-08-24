//! Plan construction and execution against real processes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use hephaestus_core::Error;
use hephaestus_tools::capabilities::{CapabilitySet, ToolCapability};
use hephaestus_verify::{Layer, LayerSpec, VerificationPlan, run_plan};

fn caps(dir: &PathBuf, cmds: &[&str]) -> CapabilitySet {
    let mut c = CapabilitySet::default()
        .with(ToolCapability::ShellExec)
        .with_workspace(dir.clone());
    for n in cmds {
        c = c.with_command(*n);
    }
    c
}

#[test]
fn plan_validation_rejects_bad_specs() {
    assert!(matches!(
        VerificationPlan::new(vec![]),
        Err(Error::Validation { .. })
    ));
    let dup = vec![
        LayerSpec::new(Layer::Format, "true", &[]),
        LayerSpec::new(Layer::Format, "true", &[]),
    ];
    assert!(VerificationPlan::new(dup).is_err());
    let pathy = vec![LayerSpec::new(Layer::Build, "/usr/bin/make", &[])];
    assert!(VerificationPlan::new(pathy).is_err());
}

#[test]
fn all_passing_plan_reports_success() {
    let dir = tempfile::tempdir().expect("tmp");
    let c = caps(&dir.path().to_path_buf(), &["echo"]);
    let plan = VerificationPlan::new(vec![
        LayerSpec::new(Layer::Format, "echo", &["fmt-ok"]),
        LayerSpec::new(Layer::Lint, "echo", &["lint-ok"]),
    ])
    .expect("plan");

    let report = run_plan(&plan, &c, false);
    assert!(report.all_passed());
    assert_eq!(report.results.len(), 2);
    assert_eq!(report.results[0].layer, "format");
    assert!(report.results[0].output_excerpt.contains("fmt-ok"));
}

#[test]
fn fail_fast_stops_at_first_failure() {
    let dir = tempfile::tempdir().expect("tmp");
    let c = caps(&dir.path().to_path_buf(), &["false", "echo"]);
    let plan = VerificationPlan::new(vec![
        LayerSpec::new(Layer::UnitTests, "echo", &["tests pass here"]),
        LayerSpec::new(Layer::Security, "false", &[]),
        LayerSpec::new(Layer::Build, "echo", &["should never run"]),
    ])
    .expect("plan");

    let report = run_plan(&plan, &c, false);
    assert!(!report.all_passed());
    assert_eq!(report.results.len(), 2, "build layer must not run");
    assert_eq!(report.results[1].layer, "security");
    assert_eq!(report.results[1].exit_code, Some(1));
}

#[test]
fn continue_on_failure_records_all_layers() {
    let dir = tempfile::tempdir().expect("tmp");
    let c = caps(&dir.path().to_path_buf(), &["false", "echo"]);
    let plan = VerificationPlan::new(vec![
        LayerSpec::new(Layer::TypeCheck, "false", &[]),
        LayerSpec::new(Layer::Build, "echo", &["built anyway"]),
    ])
    .expect("plan");

    let report = run_plan(&plan, &c, true);
    assert!(!report.all_passed());
    assert_eq!(report.results.len(), 2);
    assert!(report.results[1].passed, "later layers still execute");
}

#[test]
fn unallowlisted_layer_is_a_recorded_failure_not_a_panic() {
    let dir = tempfile::tempdir().expect("tmp");
    let c = caps(&dir.path().to_path_buf(), &["echo"]);
    let plan =
        VerificationPlan::new(vec![LayerSpec::new(Layer::Build, "make", &[])]).expect("plan");
    let report = run_plan(&plan, &c, false);
    assert!(!report.all_passed());
    assert!(
        report.results[0]
            .output_excerpt
            .contains("execution refused"),
        "refusal must be observable"
    );
}
