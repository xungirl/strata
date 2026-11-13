//! Logging and tracing setup.
//!
//! Always: human-readable logs via `tracing-subscriber` (`RUST_LOG` filter).
//! Optionally, OpenTelemetry spans for every request / search / ingest:
//! * `--otlp-endpoint http://host:4317` exports over OTLP/gRPC (batching) to
//!   a collector (Jaeger, Tempo, the OTel collector...);
//! * `--trace-stdout` prints finished spans in a human-readable form on stdout (used to verify
//!   that spans are produced without running a collector).

use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Keeps the tracer provider alive; dropping it flushes pending spans.
pub struct TelemetryGuard(Option<SdkTracerProvider>);

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = p.shutdown();
        }
    }
}

pub fn init(service: &str, otlp_endpoint: Option<&str>, stdout: bool) -> Result<TelemetryGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
    let resource = Resource::builder()
        .with_service_name(service.to_string())
        .build();
    let provider = if let Some(ep) = otlp_endpoint {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(ep)
            .build()?;
        Some(
            SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource)
                .build(),
        )
    } else if stdout {
        Some(
            SdkTracerProvider::builder()
                .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
                .with_resource(resource)
                .build(),
        )
    } else {
        None
    };
    match &provider {
        Some(p) => {
            let otel = tracing_opentelemetry::layer().with_tracer(p.tracer("strata"));
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt)
                .with(otel)
                .init();
        }
        None => tracing_subscriber::registry().with(filter).with(fmt).init(),
    }
    Ok(TelemetryGuard(provider))
}
