# ADR-005: Agent Permission Model

## Status
Accepted

## Context
Agents must be able to act, but never outside declared capabilities.
Prompt-level constraints are not enforcement.

## Decision
Capabilities are data, enforced by the runtime outside the agent:

* Each agent role declares capabilities (repository read/write, shell
  command allowlist, network egress, secret access, deployment rights).
* The tool runtime checks capability -> policy -> authorization for
  every invocation and records the decision in the audit log.
* Agents cannot rewrite the policy governing themselves: policy
  evaluation runs in the host process, fed only by configuration,
  task context and repository facts - never by model output.
* Default posture is deny: an undeclared capability fails closed.

## Consequences
* Reviewer agents cannot approve their own changes (role separation is
  encoded in policy, not prompts).
* Adding an agent role is adding a capability manifest plus prompts -
  no runtime changes.
