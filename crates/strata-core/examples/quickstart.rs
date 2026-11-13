//! Minimal end-to-end use of strata-core (same code as the README).
//! Run with `cargo run -p strata-core --example quickstart`.

use strata_core::{
    Analyzer, AnalyzerConfig, Bm25Params, InMemoryIndexBuilder, Query, Searcher, SegmentReader,
};

fn main() {
    let mut b = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::english()));
    b.add_document("d1", "Rust search engines index postings");
    b.add_document("d2", "BM25 ranks documents");
    let segment = b.build();

    // Analyze queries with the configuration the segment was built with.
    let analyzer = Analyzer::new(segment.analyzer_config().clone());
    let hits = Searcher::new(&segment, Bm25Params::ANSERINI)
        .search(&Query::bag_of_words(&analyzer, "searching engine"), 10);
    for h in &hits {
        println!("{}\t{:.4}", segment.external_id(h.doc), h.score);
    }
}
