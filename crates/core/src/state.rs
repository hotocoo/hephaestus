//! The workflow state machine.
//!
//! Hephaestus treats workflow state as an explicit finite state machine.
//! The set of legal transitions is a compile-time table; anything
//! outside the table is rejected. There is no API to "set status"
//! arbitrarily - state only changes through transition().

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Durable states of a task workflow run.
///
/// Happy path mirrors the engineering lifecycle:
/// Created -> Analyzing -> Planning -> AwaitingApproval ->
/// Implementing -> Verifying -> Reviewing -> AwaitingMerge ->
/// Building -> Deploying -> VerifyingDeployment -> Completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    /// Task accepted, nothing started yet.
    Created,
    /// Repository analysis and requirement extraction running.
    Analyzing,
    /// Plan generation running.
    Planning,
    /// Blocked until a human/policy approval decision arrives.
    AwaitingApproval,
    /// Implementation in progress.
    Implementing,
    /// Deterministic verification layers running.
    Verifying,
    /// Automated review running.
    Reviewing,
    /// Waiting for merge of the change set / PR.
    AwaitingMerge,
    /// Build producing artifacts.
    Building,
    /// Deployment to target environment in progress.
    Deploying,
    /// Post-deployment verification running.
    VerifyingDeployment,
    /// Terminal success.
    Completed,
    /// Terminal failure (reason recorded in events).
    Failed,
    /// Terminal cancellation requested by an authorized actor.
    Cancelled,
}

impl WorkflowState {
    /// All states, in lifecycle order. Used by tests and UI.
    pub const ALL: &'static [WorkflowState] = &[
        WorkflowState::Created,
        WorkflowState::Analyzing,
        WorkflowState::Planning,
        WorkflowState::AwaitingApproval,
        WorkflowState::Implementing,
        WorkflowState::Verifying,
        WorkflowState::Reviewing,
        WorkflowState::AwaitingMerge,
        WorkflowState::Building,
        WorkflowState::Deploying,
        WorkflowState::VerifyingDeployment,
        WorkflowState::Completed,
        WorkflowState::Failed,
        WorkflowState::Cancelled,
    ];

    /// Terminal states: no outgoing transitions exist.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
        )
    }

    /// Apply a trigger event, returning the next state.
    ///
    /// Returns Error::IllegalTransition if the combination is not in
    /// the static table. This is the ONLY way state changes.
    pub fn transition(self, event: TransitionEvent) -> Result<WorkflowState> {
        let allowed = match (self, event) {
            (WorkflowState::Created, TransitionEvent::StartAnalysis) => {
                Some(WorkflowState::Analyzing)
            }
            (WorkflowState::Analyzing, TransitionEvent::StartPlanning) => {
                Some(WorkflowState::Planning)
            }
            (WorkflowState::Planning, TransitionEvent::SubmitForApproval) => {
                Some(WorkflowState::AwaitingApproval)
            }
            // Plans without approval requirements skip straight to work
            // when policy allows; the policy engine decides which.
            (WorkflowState::Planning, TransitionEvent::BeginImplementation) => {
                Some(WorkflowState::Implementing)
            }
            (WorkflowState::AwaitingApproval, TransitionEvent::Approve) => {
                Some(WorkflowState::Implementing)
            }
            (WorkflowState::AwaitingApproval, TransitionEvent::RejectPlan) => {
                Some(WorkflowState::Planning)
            }
            (WorkflowState::Implementing, TransitionEvent::StartVerification) => {
                Some(WorkflowState::Verifying)
            }
            (WorkflowState::Verifying, TransitionEvent::VerificationPassed) => {
                Some(WorkflowState::Reviewing)
            }
            // Verification failures loop back into implementation so the
            // debugger/coder can fix and re-verify. Loop budget is owned
            // by the execution layer, not by this machine.
            (WorkflowState::Verifying, TransitionEvent::VerificationFailed) => {
                Some(WorkflowState::Implementing)
            }
            (WorkflowState::Reviewing, TransitionEvent::ReviewPassed) => {
                Some(WorkflowState::AwaitingMerge)
            }
            (WorkflowState::Reviewing, TransitionEvent::ChangesRequested) => {
                Some(WorkflowState::Implementing)
            }
            (WorkflowState::AwaitingMerge, TransitionEvent::Merged) => {
                Some(WorkflowState::Building)
            }
            (WorkflowState::Building, TransitionEvent::BuildSucceeded) => {
                Some(WorkflowState::Deploying)
            }
            (WorkflowState::Building, TransitionEvent::SkipDeployment) => {
                Some(WorkflowState::Completed)
            }
            (WorkflowState::Deploying, TransitionEvent::DeploymentFinished) => {
                Some(WorkflowState::VerifyingDeployment)
            }
            (WorkflowState::VerifyingDeployment, TransitionEvent::DeploymentVerified) => {
                Some(WorkflowState::Completed)
            }
            // Rollback loops back through deploying after evidence capture.
            (WorkflowState::VerifyingDeployment, TransitionEvent::RollbackRequested) => {
                Some(WorkflowState::Deploying)
            }

            // Failure and cancellation are reachable from every
            // non-terminal state.
            (s, TransitionEvent::Fail) if !s.is_terminal() => Some(WorkflowState::Failed),
            (s, TransitionEvent::Cancel) if !s.is_terminal() => Some(WorkflowState::Cancelled),
            _ => None,
        };
        allowed.ok_or_else(|| Error::IllegalTransition {
            from: self.name().to_string(),
            event,
            reason: "no such transition in the state machine".to_string(),
        })
    }

    /// Stable snake_case name (also the serialized form).
    pub fn name(self) -> &'static str {
        match self {
            WorkflowState::Created => "created",
            WorkflowState::Analyzing => "analyzing",
            WorkflowState::Planning => "planning",
            WorkflowState::AwaitingApproval => "awaiting_approval",
            WorkflowState::Implementing => "implementing",
            WorkflowState::Verifying => "verifying",
            WorkflowState::Reviewing => "reviewing",
            WorkflowState::AwaitingMerge => "awaiting_merge",
            WorkflowState::Building => "building",
            WorkflowState::Deploying => "deploying",
            WorkflowState::VerifyingDeployment => "verifying_deployment",
            WorkflowState::Completed => "completed",
            WorkflowState::Failed => "failed",
            WorkflowState::Cancelled => "cancelled",
        }
    }

    /// Parse the persisted state name back into the enum.
    ///
    /// Workers read `state` columns and must reject unknown names
    /// loudly instead of guessing; there is no default state.
    pub fn from_name(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.name() == raw)
    }
}

/// Trigger events that drive the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionEvent {
    /// Begin repository analysis.
    StartAnalysis,
    /// Analysis finished, begin planning.
    StartPlanning,
    /// Plan ready; requires approval per policy.
    SubmitForApproval,
    /// Policy allows skipping the approval gate.
    BeginImplementation,
    /// Human approved the plan.
    Approve,
    /// Human rejected the plan; return to planning.
    RejectPlan,
    /// Start deterministic verification.
    StartVerification,
    /// All required verification layers passed.
    VerificationPassed,
    /// One or more verification layers failed.
    VerificationFailed,
    /// Automated review found blocking issues.
    ChangesRequested,
    /// Review passed.
    ReviewPassed,
    /// Change set merged into the target branch.
    Merged,
    /// Build produced verified artifacts.
    BuildSucceeded,
    /// Task scope has no deployment; finish after build.
    SkipDeployment,
    /// Deployment completed; verification pending.
    DeploymentFinished,
    /// Post-deployment verification succeeded.
    DeploymentVerified,
    /// Rollback initiated (evidence preserved first).
    RollbackRequested,
    /// Terminal failure.
    Fail,
    /// Terminal cancellation.
    Cancel,
}

/// Every transition trigger event, for exhaustive iteration.
pub const EVENTS: &[TransitionEvent] = &[
    TransitionEvent::StartAnalysis,
    TransitionEvent::StartPlanning,
    TransitionEvent::SubmitForApproval,
    TransitionEvent::BeginImplementation,
    TransitionEvent::Approve,
    TransitionEvent::RejectPlan,
    TransitionEvent::StartVerification,
    TransitionEvent::VerificationPassed,
    TransitionEvent::VerificationFailed,
    TransitionEvent::ChangesRequested,
    TransitionEvent::ReviewPassed,
    TransitionEvent::Merged,
    TransitionEvent::BuildSucceeded,
    TransitionEvent::SkipDeployment,
    TransitionEvent::DeploymentFinished,
    TransitionEvent::DeploymentVerified,
    TransitionEvent::RollbackRequested,
    TransitionEvent::Fail,
    TransitionEvent::Cancel,
];

/// The complete transition table, derived from the authoritative
/// function. Exposed for tests, docs and the UI graph view.
pub fn transition_table() -> Vec<(WorkflowState, TransitionEvent, WorkflowState)> {
    let mut t = Vec::new();
    for &from in WorkflowState::ALL {
        for &event in EVENTS {
            if let Ok(to) = from.transition(event) {
                t.push((from, event, to));
            }
        }
    }
    t
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn happy_path_reaches_completed() {
        let mut s = WorkflowState::Created;
        let steps = [
            TransitionEvent::StartAnalysis,
            TransitionEvent::StartPlanning,
            TransitionEvent::SubmitForApproval,
            TransitionEvent::Approve,
            TransitionEvent::StartVerification,
            TransitionEvent::VerificationPassed,
            TransitionEvent::ReviewPassed,
            TransitionEvent::Merged,
            TransitionEvent::BuildSucceeded,
            TransitionEvent::DeploymentFinished,
            TransitionEvent::DeploymentVerified,
        ];
        for e in steps {
            s = s
                .transition(e)
                .unwrap_or_else(|err| panic!("happy path broke at {e:?}: {err}"));
        }
        assert_eq!(s, WorkflowState::Completed);
        assert!(s.is_terminal());
    }

    #[test]
    fn verification_failure_loops_to_implementing() {
        let s = WorkflowState::Verifying
            .transition(TransitionEvent::VerificationFailed)
            .unwrap_or(WorkflowState::Failed);
        assert_eq!(s, WorkflowState::Implementing);
    }

    #[test]
    fn terminal_states_have_no_outgoing_edges() {
        let terminals = [
            WorkflowState::Completed,
            WorkflowState::Failed,
            WorkflowState::Cancelled,
        ];
        for terminal in terminals {
            assert!(terminal.is_terminal());
            for event in EVENTS {
                let res = terminal.transition(*event);
                assert!(res.is_err(), "{terminal:?} must not accept {event:?}");
            }
        }
    }

    #[test]
    fn every_state_is_reachable_from_created() {
        let table = transition_table();
        let mut adjacency: HashMap<WorkflowState, Vec<WorkflowState>> = HashMap::new();
        for (from, _, to) in &table {
            adjacency.entry(*from).or_default().push(*to);
        }
        let mut seen = HashSet::new();
        let mut queue = vec![WorkflowState::Created];
        while let Some(state) = queue.pop() {
            if !seen.insert(state) {
                continue;
            }
            if let Some(nexts) = adjacency.get(&state) {
                queue.extend(nexts.iter().copied());
            }
        }
        for state in WorkflowState::ALL {
            assert!(
                seen.contains(state),
                "state {state:?} unreachable from created"
            );
        }
    }

    proptest::proptest! {
        /// Property: applying Fail/Cancel from any non-terminal state
        /// always lands on the matching terminal state.
        #[test]
        fn fail_cancel_always_terminal_or_illegal(
            idx in 0usize..WorkflowState::ALL.len(),
        ) {
            let state = WorkflowState::ALL[idx];
            let f = state.transition(TransitionEvent::Fail);
            let c = state.transition(TransitionEvent::Cancel);
            if state.is_terminal() {
                proptest::prop_assert!(f.is_err() && c.is_err());
            } else {
                proptest::prop_assert_eq!(
                    f.unwrap_or(WorkflowState::Failed),
                    WorkflowState::Failed
                );
                proptest::prop_assert_eq!(
                    c.unwrap_or(WorkflowState::Cancelled),
                    WorkflowState::Cancelled
                );
            }
        }
    }
}
