//! The model-provider boundary.
//!
//! Providers answer prompts - nothing more. They receive serialized
//! conversation state and return text; they can never execute tools,
//! touch capabilities, or observe credentials outside their own
//! configuration. Transport failures map onto the shared error
//! taxonomy as external errors; API keys are sent only as bearer
//! headers and never appear in error messages or logs.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use hephaestus_core::{Error, Result};

/// Author of a chat message on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptRole {
    /// Host-authored operating instructions.
    System,
    /// Runtime-supplied user/objective content.
    User,
    /// Prior provider output.
    Assistant,
}

impl PromptRole {
    /// Canonical lowercase wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            PromptRole::System => "system",
            PromptRole::User => "user",
            PromptRole::Assistant => "assistant",
        }
    }
}

/// One message in a provider conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Who authored this message.
    pub role: PromptRole,
    /// Message body.
    pub content: String,
}

impl ChatMessage {
    /// Convenience constructor for runtime notices and observations.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::User,
            content: content.into(),
        }
    }
}

/// A single completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Model identifier the provider should serve.
    pub model: String,
    /// Full conversation state (system preamble included).
    pub messages: Vec<ChatMessage>,
    /// Output budget in tokens.
    pub max_output_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
}

/// A successful completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResponse {
    /// First-choice assistant text.
    pub text: String,
    /// Model that actually served the request.
    pub model: String,
    /// Prompt tokens consumed, when reported.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens produced, when reported.
    pub completion_tokens: Option<u64>,
}

/// The one door models answer through.
///
/// Object-safe via explicit boxing, mirroring the engine's handler
/// trait style; providers are shared as [std::sync::Arc] instances.
pub trait ModelProvider: Send + Sync {
    /// Complete one request. Implementations must be safe to call
    /// concurrently.
    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse>> + Send + 'a>>;

    /// Stable provider name for logs and audit detail.
    fn name(&self) -> &str;
}

/// Connection settings for an OpenAI-compatible endpoint.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Base URL such as https://api.example.internal/v1.
    pub base_url: String,
    /// Bearer credential, supplied out of band. Never logged.
    pub api_key: Option<String>,
    /// Default model identifier.
    pub model: String,
    /// Per-request wall-clock timeout.
    pub timeout: Duration,
    /// Extra attempts after the first on retryable failures.
    pub max_retries: u32,
}

/// Client for any OpenAI-compatible chat-completions endpoint
/// (self-hosted gateways included). Retries bounded, fail-closed.
pub struct OpenAiCompatProvider {
    config: ProviderConfig,
    http: reqwest::Client,
}

impl OpenAiCompatProvider {
    /// Build a provider, validating configuration up front.
    pub fn new(config: ProviderConfig) -> Result<Self> {
        if config.model.trim().is_empty() {
            return Err(Error::Validation {
                field: "model".into(),
                message: "provider model must be set".into(),
            });
        }
        if !(config.base_url.starts_with("http://") || config.base_url.starts_with("https://")) {
            return Err(Error::Validation {
                field: "base_url".into(),
                message: "base_url must be an http(s) URL".into(),
            });
        }
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| Error::Config(format!("provider client build failed: {e}")))?;
        Ok(Self { config, http })
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

impl ModelProvider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        "openai-compat"
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResponse>> + Send + 'a>> {
        Box::pin(async move {
            if req.messages.is_empty() {
                return Err(Error::Validation {
                    field: "messages".into(),
                    message: "completion requires at least one message".into(),
                });
            }

            let wire_messages: Vec<WireMessage> = req
                .messages
                .iter()
                .map(|m| WireMessage {
                    role: m.role.as_str().to_string(),
                    content: m.content.clone(),
                })
                .collect();
            let body = serde_json::json!({
                "model": req.model,
                "messages": wire_messages,
                "max_tokens": req.max_output_tokens,
                "temperature": req.temperature,
            });

            let total_attempts = self.config.max_retries.saturating_add(1);
            let mut last_err: Option<Error> = None;

            for attempt in 0..total_attempts {
                if attempt > 0 {
                    // Linear backoff between retry attempts.
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                }

                let mut built = self.http.post(self.endpoint());
                if let Some(key) = &self.config.api_key {
                    built = built.bearer_auth(key);
                }
                built = built.json(&body);

                let response = match built.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        // Transport-level problem: retryable.
                        last_err = Some(external(e.to_string()));
                        continue;
                    }
                };

                let status = response.status();
                if status.is_success() {
                    let parsed: WireResponse = match response.json().await {
                        Ok(p) => p,
                        Err(e) => {
                            return Err(external(format!("malformed provider response: {e}")));
                        }
                    };
                    let text = parsed
                        .choices
                        .first()
                        .and_then(|c| c.message.content.clone())
                        .ok_or_else(|| external("provider returned no choices"))?;
                    if text.is_empty() {
                        return Err(external("provider returned an empty completion"));
                    }
                    return Ok(CompletionResponse {
                        text,
                        model: parsed.model.unwrap_or_else(|| req.model.clone()),
                        prompt_tokens: parsed.usage.as_ref().and_then(|u| u.prompt_tokens),
                        completion_tokens: parsed.usage.as_ref().and_then(|u| u.completion_tokens),
                    });
                }

                if status.as_u16() == 429 || status.is_server_error() {
                    last_err = Some(external(format!(
                        "provider temporarily unavailable (status {})",
                        status.as_u16()
                    )));
                    continue;
                }

                // Non-retryable rejection (auth, bad request, ...).
                return Err(external(format!(
                    "provider rejected the request (status {})",
                    status.as_u16()
                )));
            }

            Err(last_err.unwrap_or_else(|| external("retry budget exhausted")))
        })
    }
}

fn external(message: impl std::fmt::Display) -> Error {
    Error::External {
        system: "model-provider",
        source: Box::new(std::io::Error::other(message.to_string())),
    }
}

// ---- OpenAI-compatible wire types -------------------------------------

#[derive(Serialize)]
struct WireMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct WireResponse {
    choices: Vec<WireChoice>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessageBody,
}

#[derive(Deserialize)]
struct WireMessageBody {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn config_for(port: u16) -> ProviderConfig {
        ProviderConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: Some("test-key".into()),
            model: "forge-model".into(),
            timeout: Duration::from_secs(5),
            max_retries: 2,
        }
    }

    /// Local HTTP responder serving one canned response per connection.
    fn spawn_mock(responses: Vec<(u16, String)>) -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = Arc::clone(&hits);
        std::thread::spawn(move || {
            for (code, body) in responses {
                let (mut stream, _) = match listener.accept() {
                    Ok(x) => x,
                    Err(_) => return,
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let http = format!(
                    "HTTP/1.1 {code} T\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(http.as_bytes());
            }
        });
        (port, hits)
    }

    fn success_body() -> String {
        serde_json::json!({
            "model": "forge-model",
            "choices": [{
                "message": {"role": "assistant", "content": "the answer"}
            }],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        })
        .to_string()
    }

    #[tokio::test]
    async fn parses_success_shape_with_usage() {
        let (port, hits) = spawn_mock(vec![(200, success_body())]);
        let p = OpenAiCompatProvider::new(config_for(port)).expect("cfg");
        let req = CompletionRequest {
            model: "forge-model".into(),
            messages: vec![ChatMessage {
                role: PromptRole::System,
                content: "sys".into(),
            }],
            max_output_tokens: 64,
            temperature: 0.2,
        };
        let out = p.complete(req).await.expect("complete");
        assert_eq!(out.text, "the answer");
        assert_eq!(out.model, "forge-model");
        assert_eq!(out.prompt_tokens, Some(11));
        assert_eq!(out.completion_tokens, Some(7));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_server_errors_then_succeeds() {
        let (port, hits) = spawn_mock(vec![
            (500, "{}".into()),
            (503, "{}".into()),
            (200, success_body()),
        ]);
        let p = OpenAiCompatProvider::new(config_for(port)).expect("cfg");
        let req = CompletionRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("go")],
            max_output_tokens: 16,
            temperature: 0.1,
        };
        let out = p.complete(req).await.expect("eventual success");
        assert_eq!(out.text, "the answer");
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn unauthorized_fails_immediately_without_retry() {
        let (port, hits) = spawn_mock(vec![(401, r#"{"error":"nope"}"#.into())]);
        let p = OpenAiCompatProvider::new(config_for(port)).expect("cfg");
        let req = CompletionRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            max_output_tokens: 8,
            temperature: 0.1,
        };
        let err = p.complete(req).await.expect_err("must fail");
        // Detail lives in the source chain by design; Display stays safe.
        match &err {
            Error::External { system, source } => {
                assert_eq!(*system, "model-provider");
                let msg = source.to_string();
                assert!(msg.contains("rejected the request"), "got: {msg}");
                assert!(!msg.contains("test-key"), "key must never leak");
            }
            other => panic!("unexpected error kind: {other}"),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1, "no retry on 4xx");
    }

    #[tokio::test]
    async fn malformed_success_body_is_an_external_error() {
        let (port, hits) = spawn_mock(vec![(200, "not json at all".into())]);
        let p = OpenAiCompatProvider::new(config_for(port)).expect("cfg");
        let req = CompletionRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            max_output_tokens: 8,
            temperature: 0.1,
        };
        let err = p.complete(req).await.expect_err("must fail");
        assert!(matches!(
            err,
            Error::External {
                system: "model-provider",
                ..
            }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_budget_exhaustion_is_reported() {
        let (port, hits) = spawn_mock(vec![
            (500, "{}".into()),
            (500, "{}".into()),
            (500, "{}".into()),
        ]);
        let cfg = ProviderConfig {
            max_retries: 2,
            ..config_for(port)
        };
        let p = OpenAiCompatProvider::new(cfg).expect("cfg");
        let req = CompletionRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            max_output_tokens: 8,
            temperature: 0.1,
        };
        let err = p.complete(req).await.expect_err("must fail");
        match &err {
            Error::External { source, .. } => {
                assert!(
                    source.to_string().contains("temporarily unavailable"),
                    "got: {source}"
                );
            }
            other => panic!("unexpected error kind: {other}"),
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3, "initial plus two retries");
    }

    #[test]
    fn empty_messages_rejected_before_any_network_call() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("rt");
        let p = OpenAiCompatProvider::new(ProviderConfig {
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: None,
            model: "m".into(),
            timeout: Duration::from_secs(1),
            max_retries: 0,
        })
        .expect("cfg");
        let req = CompletionRequest {
            model: "m".into(),
            messages: Vec::new(),
            max_output_tokens: 4,
            temperature: 0.1,
        };
        let err = rt.block_on(p.complete(req)).expect_err("must fail");
        assert!(matches!(err, Error::Validation { .. }));
    }

    #[test]
    fn configuration_is_validated_up_front() {
        let mut bad = config_for(1);
        bad.model = String::new();
        assert!(OpenAiCompatProvider::new(bad).is_err());

        let mut bad2 = config_for(1);
        bad2.base_url = "ftp://x".into();
        assert!(OpenAiCompatProvider::new(bad2).is_err());
    }
}
