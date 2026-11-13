//! Prometheus metrics, exposed at `GET /metrics` in the text format.

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};

pub struct Metrics {
    registry: Registry,
    pub requests: IntCounterVec,
    pub latency: HistogramVec,
    pub ingested_docs: IntCounter,
    pub live_docs: IntGauge,
    pub segments: IntGauge,
    pub vectors: IntGauge,
}

impl Metrics {
    pub fn new() -> Metrics {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new("strata_requests_total", "Requests by route and status"),
            &["route", "status"],
        )
        .unwrap();
        // 50 µs .. ~3.3 s, doubling: covers in-process search up to slow ingest batches.
        let buckets = prometheus::exponential_buckets(50e-6, 2.0, 17).unwrap();
        let latency = HistogramVec::new(
            HistogramOpts::new(
                "strata_request_duration_seconds",
                "Request latency by route",
            )
            .buckets(buckets),
            &["route"],
        )
        .unwrap();
        let ingested_docs =
            IntCounter::new("strata_ingested_docs_total", "Documents acknowledged").unwrap();
        let live_docs =
            IntGauge::new("strata_live_docs", "Live documents in the current snapshot").unwrap();
        let segments =
            IntGauge::new("strata_segments", "Segments in the current snapshot").unwrap();
        let vectors = IntGauge::new(
            "strata_vectors",
            "Vectors in the HNSW graph (incl. replaced ones)",
        )
        .unwrap();
        for c in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(latency.clone()),
            Box::new(ingested_docs.clone()),
            Box::new(live_docs.clone()),
            Box::new(segments.clone()),
            Box::new(vectors.clone()),
        ] {
            registry.register(c).unwrap();
        }
        Metrics {
            registry,
            requests,
            latency,
            ingested_docs,
            live_docs,
            segments,
            vectors,
        }
    }

    /// Render all metrics in the Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut buf)
            .unwrap();
        String::from_utf8(buf).unwrap()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
