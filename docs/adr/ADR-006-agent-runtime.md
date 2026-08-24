# ADR-006: Agent Runtime and Model Boundary

## Status
Accepted

## Context
Agents must act on repositories under exactly the capabilities their
role declares (ADR-005), while model providers are external systems
that answer prompts. The runtime needs one audited path from "model
said something" to "something happened", plus role definitions whose
constraints survive any prompt content.

## Decision
Three fail-closed pieces:

1. Role manifests are data: registered tool names, capability grants,
   and command allowlists. Validation refuses manifests violating
   role policy - reviewers and planners are read-only, verifiers
   cannot write sources, no current role holds secret/network/deploy
   grants, every grant serves an allowed tool, and unknown tool names
   or non-bare commands fail validation loudly.
2. Model providers implement a single boxed-future completion trait.
   An OpenAI-compatible client ships first; transport failures map to
   the shared error taxonomy with bounded retries on 429/5xx only,
   and credentials never appear in errors or logs.
3. The session loop is the only way agents act. Models answer in a
   strict single-JSON-object protocol (final answer OR one tool call),
   parsed deterministically with no function-calling API coupling.
   Every call passes capability authorization and is recorded through
   a DecisionSink into the tamper-evident audit log before execution;
   audit failure fails the session. Observations are secret-redacted,
   truncated, and re-framed as untrusted data. Denials are counted and
   stop the run when systematic; turn and tool-call budgets bound cost.

There is no path from model output to capability state.

## Consequences
* Adding an agent role is a reviewed-code change to the manifest enum
  plus its separation rules - never configuration alone.
* Provider swap (local gateways, hosted models) requires no runtime
  changes; enforcement does not depend on provider behavior.
* Planning/implementation handlers register against this runtime in a
  later phase; the loop and its guarantees stay unchanged.