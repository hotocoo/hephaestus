# ADR-003: Durable Workflow Engine

## Status
Accepted

## Context
Engineering tasks run for minutes to hours across processes that can
crash at any time. Arbitrary "set status" APIs invite corruption.

## Decision
Workflow state is an explicit finite state machine in hephaestus-core
(total function from (state, event) to next state). Every transition:

1. is validated by the pure function first,
2. is persisted transactionally together with its event record,
3. carries the actor and trigger for audit.

Workers claim steps via leases; expired leases are recoverable. There
is no API that sets workflow status directly - only legal transitions.

Verification failures loop back to implementation with a bounded retry
budget owned by the execution layer, keeping the machine acyclic except
for documented fix loops.

## Consequences
* Illegal states are unrepresentable; recovery after crash means
  re-reading persisted state and resuming from the last checkpoint.
* New lifecycle stages require a migration of both the table and any
  in-flight runs (documented upgrade path required).
