//! Static dashboard serving (ADR-014).
//!
//! Serves the built Vue dashboard from disk next to the API so one
//! process fronts both surfaces, same-origin by default. The bundle's
//! bootstrap placeholder is replaced once at load with the configured
//! runtime configuration; a bundle without the expected placeholder
//! fails startup loudly instead of silently ignoring its settings.
//! Everything under /api keeps the JSON error contract: the static
//! fallback never swallows API paths, and no operations are added to
//! the OpenAPI inventory.
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use hephaestus_config::WebRuntimeConfig;
use hephaestus_core::{Error, Result};

/// Exact bootstrap statement the built index.html must contain when
/// runtime configuration is injected (matches apps/web/index.html).
const BOOTSTRAP_PLACEHOLDER: &str =
    "window.__HEPHAESTUS_WEB_CONFIG__ = window.__HEPHAESTUS_WEB_CONFIG__ ?? {};";

/// A loaded dashboard site: the dist root plus its rewritten entry.
#[derive(Debug, Clone)]
pub struct WebSite {
    /// Canonical dist directory every served path resolves under.
    dist: PathBuf,
    /// index.html bytes with runtime configuration applied.
    index_html: Arc<Vec<u8>>,
}

impl WebSite {
    /// Load the site from a configured dist directory.
    ///
    /// Fails closed on a missing or malformed bundle: an operator who
    /// asked for serving must not get silence, and injected runtime
    /// configuration must never be dropped on the floor.
    pub fn load(dist_dir: &Path, runtime_config: Option<&WebRuntimeConfig>) -> Result<Self> {
        let canonical = std::fs::canonicalize(dist_dir).map_err(|e| {
            Error::Config(format!(
                "web.dist_dir {} is not reachable: {e}",
                dist_dir.display()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(Error::Config(format!(
                "web.dist_dir {} is not a directory",
                dist_dir.display()
            )));
        }
        let index_path = canonical.join("index.html");
        let raw = std::fs::read(&index_path).map_err(|e| {
            Error::Config(format!(
                "web.dist_dir is missing index.html ({}): {e}",
                index_path.display()
            ))
        })?;
        let index_html = match runtime_config {
            None => raw,
            Some(rc) => rewrite_index(&raw, rc)?,
        };
        Ok(Self {
            dist: canonical,
            index_html: Arc::new(index_html),
        })
    }

    /// Resolve one request path inside the dist tree.
    ///
    /// Returns None for anything that would escape the root or does
    /// not exist; the caller decides between SPA fallback and a 404.
    fn resolve(&self, request_path: &str) -> Option<PathBuf> {
        let rel = request_path.trim_start_matches('/');
        if rel.is_empty() {
            return None;
        }
        // Windows-style separators never traverse here either.
        if rel.contains('\\') || rel.contains('\0') {
            return None;
        }
        let candidate = PathBuf::from(rel);
        let normal = candidate
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        if !normal {
            return None;
        }
        let full = self.dist.join(candidate);
        if full.is_file() { Some(full) } else { None }
    }

    /// Serve one GET/HEAD path: exact file, or the SPA entry.
    pub async fn respond(&self, method: &Method, path: &str) -> Response {
        if !matches!(method, &Method::GET | &Method::HEAD) {
            return plain_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        }
        if let Some(file) = self.resolve(path) {
            match tokio::fs::read(&file).await {
                Ok(bytes) => return file_response(bytes, &file),
                Err(err) => {
                    tracing::warn!(error = %err, path = %file.display(), "static read failed");
                    return plain_response(StatusCode::NOT_FOUND, "not found");
                }
            }
        }
        // Missing hashed assets must 404 rather than masquerade as HTML.
        if path.trim_start_matches('/').starts_with("assets/") {
            return plain_response(StatusCode::NOT_FOUND, "asset not found");
        }
        // Dot segments never get the SPA fallback either: traversal
        // attempts and their normalized cousins deserve a hard no, not
        // the entry document.
        if has_dot_segments(path) || path.contains('\\') {
            return plain_response(StatusCode::NOT_FOUND, "not found");
        }
        // SPA fallback: unknown client-side routes render the entry.
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from((*self.index_html).clone()))
            .unwrap_or_else(|_| plain_response(StatusCode::INTERNAL_SERVER_ERROR, "render failed"))
    }
}

/// Whether any raw path segment is a dot segment (`.` / `..`).
fn has_dot_segments(path: &str) -> bool {
    path.split('/').any(|seg| seg == "." || seg == "..")
}

/// Replace the bundle's bootstrap placeholder with configured values.
fn rewrite_index(raw: &[u8], rc: &WebRuntimeConfig) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(raw).into_owned();
    let injected = serde_json::json!({
        "baseUrl": rc.base_url,
        "token": rc.token,
        "pollSeconds": rc.poll_seconds,
    });
    let statement = format!(
        "window.__HEPHAESTUS_WEB_CONFIG__ = Object.assign(window.__HEPHAESTUS_WEB_CONFIG__ ?? {{}}, {injected});"
    );
    if !text.contains(BOOTSTRAP_PLACEHOLDER) {
        return Err(Error::Config(
            "web.dist_dir/index.html lacks the Hephaestus bootstrap placeholder; \
             refusing to serve a dashboard that would ignore its runtime config"
                .into(),
        ));
    }
    Ok(text
        .replacen(BOOTSTRAP_PLACEHOLDER, &statement, 1)
        .into_bytes())
}

/// Content type for a served file, by extension.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Vite emits content-hashed names under assets/ - safe to cache hard.
fn cache_control(path: &Path) -> &'static str {
    match path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        Some("assets") => "public, max-age=31536000, immutable",
        _ => "public, max-age=300",
    }
}

fn file_response(bytes: Vec<u8>, path: &Path) -> Response {
    match Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, header_value(content_type(path)))
        .header(header::CACHE_CONTROL, header_value(cache_control(path)))
        .body(Body::from(bytes))
    {
        Ok(response) => response,
        Err(_) => plain_response(StatusCode::INTERNAL_SERVER_ERROR, "render failed"),
    }
}

fn plain_response(status: StatusCode, message: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(message))
        .unwrap_or_else(|_| {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or(HeaderValue::from_static("application/octet-stream"))
}
