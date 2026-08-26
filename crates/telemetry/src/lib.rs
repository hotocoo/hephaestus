//! # hephaestus-telemetry
//!
//! Process observability for the Hephaestus binaries (ADR-014).
//!
//! One path installs structured logging for every process and, when
//! configuration names an OTLP endpoint, wires real span export over
//! OTLP/HTTP (protobuf) tagged with the configured service name. An
//! absent endpoint means local logging only - the honest default for
//! laptops - while a malformed one fails startup loudly: the endpoint
//! setting either produces exported telemetry or refuses to start.
//!
//! The crate also owns the shutdown signal contract shared by the
//! server and the worker: Ctrl-C and SIGTERM resolve identically, so
//! process managers get the same graceful drain interactive use has.
//!
//! Design rules:
//! * Setup happens once per process, as the first step inside the
//!   binary's runtime.
//! * Export batches on the SDK's dedicated background thread using a
//!   blocking HTTP client; the returned guard shuts the tracer
//!   provider down explicitly so in-flight spans flush.
//! * Nothing here holds authority or state; it configures plumbing.

use std::time::Duration;

use hephaestus_config::TelemetryConfig;
use hephaestus_core::{Error, Result};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Path appended to a configured base endpoint for span export.
const TRACES_PATH: &str = "/v1/traces";

/// Wall-clock budget for one OTLP export attempt.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Process-wide telemetry installation.
///
/// Holds the tracer provider so it can be shut down explicitly,
/// flushing batched spans before process exit.
#[derive(Debug)]
pub struct Telemetry {
    provider: Option<SdkTracerProvider>,
}

impl Telemetry {
    /// Flush and shut down the export pipeline.
    ///
    /// Safe to call more than once; a logging-only installation is a
    /// no-op.
    pub fn shutdown(self) {
        if let Some(provider) = self.provider
            && let Err(err) = provider.shutdown()
        {
            eprintln!("telemetry shutdown warning: {err}");
        }
    }
}

/// Install process telemetry from configuration (ADR-014).
///
/// Structured logging is always installed; setting
/// `telemetry.otlp_endpoint` additionally exports spans over
/// OTLP/HTTP with the configured service name as resource attribute.
pub fn init(cfg: &TelemetryConfig) -> Result<Telemetry> {
    let filter = EnvFilter::try_new(cfg.log_level.clone())
        .map_err(|e| Error::Config(format!("telemetry.log_level invalid: {e}")))?;

    let Some(endpoint) = cfg.otlp_endpoint.as_deref() else {
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(filter))
            .init();
        return Ok(Telemetry { provider: None });
    };

    let trace_url = resolve_trace_endpoint(endpoint)?;
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(trace_url)
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|e| Error::Storage(Box::new(e)))?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_attribute(opentelemetry::KeyValue::new(
            "service.name",
            cfg.service_name.clone(),
        ))
        .build();

    let processor = BatchSpanProcessor::builder(exporter).build();
    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_span_processor(processor)
        .build();
    // Keep a handle alive for explicit flush-on-shutdown; the global
    // registration lets any library-level OpenTelemetry API join in.
    global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer(cfg.service_name.clone());

    let otel_layer: OpenTelemetryLayer<_, opentelemetry_sdk::trace::Tracer> =
        tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(otel_layer.with_filter(filter))
        .with(tracing_subscriber::fmt::layer())
        .init();

    Ok(Telemetry {
        provider: Some(provider),
    })
}

/// Resolve the exact span-export URL from a configured base endpoint.
///
/// Operators give collector roots (`http://host:4318`); the OTLP/HTTP
/// protocol fixes the traces path. An explicit `.../v1/traces` passes
/// through untouched. Only http(s) schemes are accepted.
fn resolve_trace_endpoint(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::Config(
            "telemetry.otlp_endpoint must not be empty when set".into(),
        ));
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|e| Error::Config(format!("telemetry.otlp_endpoint invalid URL: {e}")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(Error::Config(format!(
                "telemetry.otlp_endpoint must be an http(s) URL, got scheme {other:?}"
            )));
        }
    }
    if parsed.path().ends_with(TRACES_PATH) {
        return Ok(trimmed.to_string());
    }
    let mut joined = trimmed.trim_end_matches('/').to_string();
    joined.push_str(TRACES_PATH);
    Ok(joined)
}

/// Resolve when the operator asks the process to stop.
///
/// Ctrl-C and SIGTERM are equivalent shutdown requests (ADR-014):
/// process managers stop services with SIGTERM, and both binaries owe
/// them the same graceful drain interactive use gets.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            // A process that cannot watch SIGTERM still watches Ctrl-C.
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn empty_endpoint_is_rejected() {
        let err = resolve_trace_endpoint("   ").expect_err("empty must fail");
        assert!(err.to_string().contains("must not be empty"), "got: {err}");
    }

    #[test]
    fn non_http_scheme_is_rejected() {
        let err = resolve_trace_endpoint("grpc://collector:4317").expect_err("scheme must fail");
        assert!(err.to_string().contains("http(s)"), "got: {err}");
    }

    #[test]
    fn unparseable_endpoint_is_rejected() {
        let err = resolve_trace_endpoint("not a url").expect_err("parse must fail");
        assert!(err.to_string().contains("invalid URL"), "got: {err}");
    }

    #[test]
    fn base_endpoint_gains_fixed_traces_path() {
        assert_eq!(
            resolve_trace_endpoint("http://127.0.0.1:4318").expect("resolve"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            resolve_trace_endpoint("http://127.0.0.1:4318/").expect("resolve"),
            "http://127.0.0.1:4318/v1/traces"
        );
    }

    #[test]
    fn explicit_traces_path_passes_through() {
        assert_eq!(
            resolve_trace_endpoint("https://gw.internal/otlp/v1/traces").expect("resolve"),
            "https://gw.internal/otlp/v1/traces"
        );
    }

    /// A synchronous one-shot OTLP/HTTP collector for export assertions.
    struct CapturedRequest {
        path: String,
        content_type: Option<String>,
        body_len: usize,
    }

    fn serve_one_export(
        listener: std::net::TcpListener,
    ) -> std::thread::JoinHandle<CapturedRequest> {
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).expect("read head");
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if n == 0 {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next().unwrap_or_default();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let content_type = lines.clone().find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-type")
                    .then(|| value.trim().to_string())
            });
            let content_length = lines
                .clone()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let header_end = head.find("\r\n\r\n").expect("head end") + 4;
            let mut body = buf[header_end..].to_vec();
            while body.len() < content_length {
                let n = stream.read(&mut chunk).expect("read body");
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nOK")
                .expect("respond");
            CapturedRequest {
                path,
                content_type,
                body_len: body.len(),
            }
        })
    }

    /// The configured endpoint produces real exported spans over
    /// OTLP/HTTP protobuf - the promise ADR-014 makes explicit. Init runs
    /// inside a tokio runtime exactly like the binaries do.
    #[test]
    fn otlp_endpoint_exports_spans_to_a_collector() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let collector = serve_one_export(listener);

        let cfg = TelemetryConfig {
            log_level: "info".into(),
            otlp_endpoint: Some(format!("http://{addr}")),
            service_name: "telemetry-export-test".into(),
        };
        // The binaries initialize telemetry inside their tokio runtime;
        // the test mirrors that exactly. Only one test may install the
        // global subscriber, so this is the sole init() call in the suite.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let telemetry = init(&cfg).expect("installs with endpoint");
            // A real span (events do not reach the span pipeline) that
            // ends before shutdown, so the batch holds exactly one item.
            tracing::info_span!("collector_probe", stage = "export").in_scope(|| {
                tracing::debug!("inside probe span");
            });
            // Shutdown flushes the batched span through the exporter.
            telemetry.shutdown();
        });

        let captured = collector.join().expect("collector thread");
        assert_eq!(captured.path, "/v1/traces", "OTLP/HTTP traces path");
        assert_eq!(
            "application/x-protobuf",
            captured.content_type.as_deref().unwrap_or(""),
            "protobuf wire format"
        );
        assert!(captured.body_len > 0, "a span payload arrived");
    }
}
