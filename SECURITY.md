# Security Policy

Hephaestus executes AI agents against real repositories and grants them
tool access under policy. Security failures here are not theoretical.
This document explains how to report vulnerabilities and what we
commit to.

## Supported versions

| Version | Supported |
|---------|-----------|
| main branch | security fixes land here first |
| tagged releases | latest minor release of each major |

Pre-1.0: only the latest release receives security patches.

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report privately to **security@hephaestus.invalid**, including:

* affected component(s) (crate, service, migration)
* a minimal reproduction or proof of concept
* impact assessment as you understand it
* whether the issue is already being exploited in the wild

You will receive an acknowledgment within **3 business days**. We aim
to provide an initial assessment within **10 business days** and will
coordinate a disclosure timeline with you; our default target for
patching is 90 days from report, sooner for actively exploited issues.

We credit reporters by name in release notes unless you prefer
anonymous attribution.

## Scope

In scope:

* Sandbox escapes or isolation bypasses
* Prompt injection that escalates to unauthorized tool execution
* Authorization bypasses, including cross-tenant access
* Secret disclosure through logs, traces, artifacts, or API responses
* SQL/command injection reachable from untrusted input
* Webhook signature forgery or replay acceptance
* Supply-chain compromises of release artifacts

Out of scope:

* Social engineering of hosting providers
* Vulnerabilities in dependencies not reachable from Hephaestus
  surfaces (report upstream; tell us so we can track it)
* Denial-of-service via authenticated resource exhaustion without
  quota bypass (we treat quota bugs as regular issues unless they
  enable worse primitives)

## Security model summary

The full model is documented in docs/adr (ADR-005 agent permissions,
ADR-012 security model) and evolves with the codebase. Non-negotiable
invariants enforced today:

1. Untrusted content is data. Repository text, issue text, tool output,
   and model output carry provenance tags; they never become
   instructions at the runtime layer.
2. Capability enforcement happens outside the agent. Policy evaluation
   never consults model output; agents cannot rewrite their own rules.
3. Fail closed. Missing authorization, unverifiable integrity, or
   broken isolation stops the operation instead of degrading.
4. Secrets are redacted from logs as defense-in-depth and are excluded
   from persistence paths by design.

## Disclosure policy

We publish advisories after a fix ships, describing impact,
affected versions, remediation, and workarounds. We do not publish
exploitation details beyond what defenders need until the majority of
upgrade windows have reasonably elapsed.
