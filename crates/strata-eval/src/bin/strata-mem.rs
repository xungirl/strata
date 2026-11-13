//! `strata-mem`: measure how much heap the in-memory index really uses.
//!
//! This binary installs an *instrumented global allocator* (`stats_alloc`),
//! which counts every byte the program allocates and frees. The difference
//! (allocated - freed) across a code region is the live heap that region
//! left behind. It lives in its own binary because counting every allocation
//! with atomic operations slows the program down, and we don't want that
//! overhead in the timing runs of `strata-eval`.
//!
//! Reported per dataset:
//! * `builder_live_bytes` - live heap after adding all docs, before `build()`
//!   (the mutable `HashMap<term, Vec<Posting>>` form)
//! * `segment_live_bytes` - live heap of the frozen `InMemorySegment` after the
//!   builder is dropped
//! * `segment_estimate_bytes` - `InMemorySegment::heap_size_bytes()`
//!   (capacity-based estimate, no allocator overhead)
//!
//! Live bytes are requested sizes as seen by `GlobalAlloc`; the system
//! allocator's own per-allocation rounding/metadata is not included.

use std::alloc::System;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use strata_core::{Analyzer, AnalyzerConfig, InMemoryIndexBuilder};
use strata_eval::beir::Dataset;

// `#[global_allocator]` replaces the allocator for the whole binary.
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Parser)]
struct Args {
    #[arg(long, required = true, num_args = 1..)]
    datasets: Vec<String>,
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
}

fn live(region: &Region<System>) -> i64 {
    let s = region.change();
    s.bytes_allocated as i64 - s.bytes_deallocated as i64
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("dataset\tdocs\tcorpus_text_bytes\tbuilder_live_bytes\tsegment_live_bytes\tsegment_estimate_bytes\tpostings\tbytes_per_posting");
    for name in &args.datasets {
        let ds = Dataset::load(&args.data_dir, name, "test")?;
        let corpus_text_bytes: usize = ds.corpus.iter().map(|d| d.title.len() + d.text.len()).sum();

        // Measure from here: the dataset itself is already loaded and excluded.
        let region = Region::new(GLOBAL);
        let mut builder = InMemoryIndexBuilder::new(Analyzer::new(AnalyzerConfig::english()));
        for doc in &ds.corpus {
            builder.add_document(&doc.id, &doc.flat_text());
        }
        let builder_live = live(&region);
        let segment = builder.build(); // consumes and drops the builder
        let segment_live = live(&region);
        let estimate = segment.heap_size_bytes();
        let postings = segment.num_postings();
        println!(
            "{name}\t{}\t{corpus_text_bytes}\t{builder_live}\t{segment_live}\t{estimate}\t{postings}\t{:.2}",
            ds.corpus.len(),
            segment_live as f64 / postings as f64
        );
        drop(segment);
    }
    Ok(())
}
