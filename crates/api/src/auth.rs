//! Bearer-key authentication.
//!
//! Keys are pre-provisioned in configuration (ADR-009): each binds one
//! bearer token to exactly one organization and one principal. There
//! is no issuance endpoint; rotation means config change plus restart.
//!
//! When auth is disabled - legal only outside production - requests
//! must still name their tenant explicitly through the
//! `X-Hephaestus-Organization` header. Tenant scope is never inferred,
//! so even trusted dev traffic stays attributable.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use hephaestus_config::ApiKey;
use hephaestus_core::id::OrganizationId;
use hephaestus_core::{Error, Result};
use uuid::Uuid;

use crate::ApiError;
use crate::state::AppState;

/// Header naming the tenant when auth is disabled.
pub const ORG_HEADER: &str = "x-hephaestus-organization";

/// An authenticated caller: tenant scope plus attributable principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Organization every store access is scoped to.
    pub organization_id: OrganizationId,
    /// Recorded on gate decisions made by this caller.
    pub principal: String,
}

impl Principal {
    /// The synthetic principal used when auth is disabled.
    pub fn anonymous(organization_id: OrganizationId) -> Self {
        Self {
            organization_id,
            principal: "anonymous".into(),
        }
    }
}

/// One provisioned key ready for lookup.
#[derive(Debug, Clone)]
struct KeyEntry {
    token: String,
    principal: Principal,
}

/// Authentication policy built from validated configuration.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    disabled: bool,
    entries: Arc<[KeyEntry]>,
}

impl AuthPolicy {
    /// Build the policy from configuration keys.
    pub fn new(disabled: bool, keys: &[ApiKey]) -> Self {
        let entries = keys
            .iter()
            .map(|k| KeyEntry {
                token: k.token.clone(),
                principal: Principal {
                    organization_id: OrganizationId::from_uuid(k.organization_id),
                    principal: k.principal.clone(),
                },
            })
            .collect();
        Self { disabled, entries }
    }

    /// Whether auth checks are skipped (development/test only).
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    /// Resolve a request's headers into a principal.
    ///
    /// Every failure is an authentication failure: unknown tokens,
    /// malformed headers and missing tenants are indistinguishable to
    /// callers on purpose.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Principal> {
        if self.disabled {
            let raw = headers
                .get(ORG_HEADER)
                .and_then(|v| v.to_str().ok())
                .ok_or(Error::Unauthenticated)?;
            let org = Uuid::parse_str(raw.trim()).map_err(|_| Error::Unauthenticated)?;
            return Ok(Principal::anonymous(OrganizationId::from_uuid(org)));
        }

        let presented = bearer_token(headers).ok_or(Error::Unauthenticated)?;
        // Full scan with accumulating match: timing does not reveal
        // whether or where a key matched. Key counts are small
        // (configuration-provisioned), so linear cost is intended.
        let mut hit: Option<&KeyEntry> = None;
        for entry in self.entries.iter() {
            if constant_time_eq(presented.as_bytes(), entry.token.as_bytes()) && hit.is_none() {
                hit = Some(entry);
            }
        }
        hit.map(|e| e.principal.clone())
            .ok_or(Error::Unauthenticated)
    }
}

/// Extract the bearer token from an Authorization header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let rest = value
        .strip_prefix("bearer ")
        .or_else(|| value.strip_prefix("Bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Byte comparison whose cost does not depend on match position.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Axum middleware: resolve the principal before any handler runs.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> std::result::Result<Response, ApiError> {
    let principal = state.auth.authenticate(req.headers())?;
    req.extensions_mut().insert(principal);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    const ORG: &str = "018f3c1e-0000-7000-8000-00000000000a";
    const OTHER_ORG: &str = "018f3c1e-0000-7000-8000-00000000000b";

    fn key(token: &str, org: &str, principal: &str) -> ApiKey {
        ApiKey {
            token: token.into(),
            organization_id: org.parse().expect("uuid"),
            principal: principal.into(),
        }
    }

    fn headers_with(headers: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        map
    }

    #[test]
    fn valid_bearer_authenticates_to_its_tenant() {
        let policy = AuthPolicy::new(false, &[key("token-one-16-chars", ORG, "ci-bot")]);
        let headers = headers_with(&[("authorization", "Bearer token-one-16-chars")]);
        let principal = policy.authenticate(&headers).expect("authenticated");
        assert_eq!(principal.principal, "ci-bot");
        assert_eq!(principal.organization_id.as_uuid().to_string(), ORG);
    }

    #[test]
    fn unknown_and_missing_tokens_are_indistinguishable() {
        let policy = AuthPolicy::new(false, &[key("token-one-16-chars", ORG, "ci-bot")]);
        let unknown = headers_with(&[("authorization", "Bearer not-a-real-token-at-all")]);
        let missing = headers_with(&[]);
        for headers in [&unknown, &missing] {
            assert!(matches!(
                policy.authenticate(headers),
                Err(Error::Unauthenticated)
            ));
        }
    }

    #[test]
    fn scheme_must_be_bearer() {
        let policy = AuthPolicy::new(false, &[key("token-one-16-chars", ORG, "ci-bot")]);
        let basic = headers_with(&[("authorization", "Basic dXNlcjpwYXNz")]);
        assert!(matches!(
            policy.authenticate(&basic),
            Err(Error::Unauthenticated)
        ));
        // Lowercase schemes are accepted.
        let lower = headers_with(&[("authorization", "bearer token-one-16-chars")]);
        assert!(policy.authenticate(&lower).is_ok());
    }

    #[test]
    fn disabled_mode_requires_explicit_organization() {
        let policy = AuthPolicy::new(true, &[]);
        let named = headers_with(&[("x-hephaestus-organization", ORG)]);
        let principal = policy.authenticate(&named).expect("anonymous");
        assert_eq!(principal.principal, "anonymous");
        assert_eq!(principal.organization_id.as_uuid().to_string(), ORG);

        let unnamed = headers_with(&[]);
        assert!(matches!(
            policy.authenticate(&unnamed),
            Err(Error::Unauthenticated)
        ));

        let garbage = headers_with(&[("x-hephaestus-organization", "not-a-uuid")]);
        assert!(matches!(
            policy.authenticate(&garbage),
            Err(Error::Unauthenticated)
        ));
    }

    #[test]
    fn enabled_mode_never_trusts_the_org_header() {
        let policy = AuthPolicy::new(false, &[key("token-one-16-chars", ORG, "ci-bot")]);
        // A header alone grants nothing when auth is enabled.
        let headers = headers_with(&[("x-hephaestus-organization", OTHER_ORG)]);
        assert!(matches!(
            policy.authenticate(&headers),
            Err(Error::Unauthenticated)
        ));
    }

    #[test]
    fn first_matching_key_wins_on_exact_duplicates() {
        let policy = AuthPolicy::new(
            false,
            &[
                key("duplicate-token-16ch", ORG, "first"),
                key("duplicate-token-16ch", OTHER_ORG, "second"),
            ],
        );
        let headers = headers_with(&[("authorization", "Bearer duplicate-token-16ch")]);
        let principal = policy.authenticate(&headers).expect("authenticated");
        // Configuration validation rejects duplicates upstream; the
        // lookup still resolves deterministically to the first entry.
        assert_eq!(principal.principal, "first");
    }
}
