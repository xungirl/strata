//! BM25 ranking function.
//!
//! For a query `Q` and document `D`:
//!
//! ```text
//! score(D, Q) = sum over terms t in Q of
//!                 qtf(t) * IDF(t) * tf * (k1 + 1) / (tf + k1 * (1 - b + b * |D| / avgdl))
//!
//! IDF(t) = ln(1 + (N - df + 0.5) / (df + 0.5))
//! ```
//!
//! * `tf`    - occurrences of `t` in `D`;  `qtf(t)` - occurrences of `t` in the query
//! * `df`    - documents containing `t`;  `N` - documents in the collection
//! * `|D|`   - length of `D` in tokens;   `avgdl` - average document length
//! * `k1`    - term-frequency saturation: how fast extra occurrences stop helping.
//!   `k1 = 0` ignores tf entirely; large `k1` makes it nearly linear.
//! * `b`     - length normalization strength: `b = 0` ignores length, `b = 1`
//!   fully normalizes by `|D| / avgdl`.
//!
//! The IDF is the "Lucene" variant (`ln(1 + ...)`), which is always positive.
//! The original Robertson/Sparck-Jones IDF `ln((N - df + 0.5) / (df + 0.5))`
//! goes *negative* for terms in more than half the documents, which can make a
//! document score lower for matching a query term. (Lucene 8+ also drops the
//! constant `(k1 + 1)` factor; it scales every score equally, so rankings are
//! identical either way. We keep it, matching the textbook and Tantivy.)

use serde::{Deserialize, Serialize};

/// How document lengths are fed into the length normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocLengthEncoding {
    /// Use the exact token count.
    Exact,
    /// Round the length the way Lucene stores it: in a single byte
    /// ("norms", `SmallFloat.intToByte4`). Exact below 24, then ~3 bits of
    /// mantissa (up to ~12% relative error). Used only to explain the gap to
    /// Anserini's numbers; costs nothing at query time in Lucene because the
    /// 256 possible values are precomputed.
    LuceneByte4,
}

/// BM25 hyper-parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bm25Params {
    pub k1: f32,
    pub b: f32,
    pub doc_length: DocLengthEncoding,
}

impl Bm25Params {
    /// Anserini's BEIR defaults (`k1 = 0.9, b = 0.4`), tuned on MS MARCO.
    pub const ANSERINI: Bm25Params = Bm25Params {
        k1: 0.9,
        b: 0.4,
        doc_length: DocLengthEncoding::Exact,
    };

    /// The "classic" textbook / Lucene / Tantivy defaults (`k1 = 1.2, b = 0.75`).
    pub const CLASSIC: Bm25Params = Bm25Params {
        k1: 1.2,
        b: 0.75,
        doc_length: DocLengthEncoding::Exact,
    };

    /// Same parameters with a different document-length encoding.
    pub fn with_doc_length(self, doc_length: DocLengthEncoding) -> Self {
        Bm25Params { doc_length, ..self }
    }
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self::ANSERINI
    }
}

/// Lucene-style BM25 inverse document frequency: `ln(1 + (N - df + 0.5) / (df + 0.5))`.
///
/// Computed in `f64` and rounded once to `f32` to avoid precision loss on
/// large collections.
pub fn idf(num_docs: u64, doc_freq: u64) -> f32 {
    let n = num_docs as f64;
    let df = doc_freq as f64;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln() as f32
}

/// Precomputed scorer for one query term. Everything that does not depend on
/// the document (IDF, `qtf`, `k1`, `b / avgdl`) is folded in once, so scoring
/// a posting is a handful of multiply/adds and one division.
#[derive(Clone, Copy, Debug)]
pub struct Bm25TermScorer {
    /// `qtf * IDF * (k1 + 1)`
    weight: f32,
    /// `k1 * (1 - b)`
    k1_one_minus_b: f32,
    /// `k1 * b / avgdl`
    k1_b_over_avgdl: f32,
    doc_length: DocLengthEncoding,
}

impl Bm25TermScorer {
    /// * `idf`        - from [`idf`]
    /// * `query_tf`   - how often the term appears in the query
    /// * `avg_doc_len`- collection average document length
    pub fn new(params: Bm25Params, idf: f32, query_tf: u32, avg_doc_len: f32) -> Self {
        Bm25TermScorer {
            weight: query_tf as f32 * idf * (params.k1 + 1.0),
            k1_one_minus_b: params.k1 * (1.0 - params.b),
            k1_b_over_avgdl: params.k1 * params.b / avg_doc_len,
            doc_length: params.doc_length,
        }
    }

    /// BM25 contribution of this term for a document with term frequency `tf`
    /// and length `doc_len`.
    #[inline]
    pub fn score(&self, tf: u32, doc_len: u32) -> f32 {
        let dl = match self.doc_length {
            DocLengthEncoding::Exact => doc_len,
            DocLengthEncoding::LuceneByte4 => lucene_quantize_length(doc_len),
        };
        let tf = tf as f32;
        let norm = self.k1_one_minus_b + self.k1_b_over_avgdl * dl as f32;
        self.weight * tf / (tf + norm)
    }
}

/// Round a document length the way Lucene's norms store it: encode into one
/// byte with `SmallFloat.intToByte4`, then decode with `byte4ToInt`.
///
/// Port of `org.apache.lucene.util.SmallFloat` (Lucene 9). Values below 24
/// are stored exactly; larger values keep their top 4 significant bits
/// (the leading 1 plus 3 mantissa bits) and are rounded *down*.
pub fn lucene_quantize_length(len: u32) -> u32 {
    // Byte values 0..NUM_FREE_VALUES encode lengths exactly; the remaining 231
    // codes encode (len - NUM_FREE_VALUES) as a tiny 4-bit float.
    const NUM_FREE_VALUES: u32 = 24;
    if len < NUM_FREE_VALUES {
        return len;
    }
    let rest = u64::from(len - NUM_FREE_VALUES);
    let decoded = int4_to_long(long_to_int4(rest));
    NUM_FREE_VALUES + decoded as u32
}

/// `SmallFloat.longToInt4`: 3 mantissa bits + exponent.
fn long_to_int4(i: u64) -> u32 {
    let num_bits = 64 - i.leading_zeros();
    if num_bits < 4 {
        i as u32 // "subnormal": stored as-is
    } else {
        let shift = num_bits - 4;
        let mantissa = ((i >> shift) as u32) & 0x07; // drop the implicit leading 1
        mantissa | ((shift + 1) << 3)
    }
}

/// `SmallFloat.int4ToLong`: inverse of [`long_to_int4`] (lossy).
fn int4_to_long(i: u32) -> u64 {
    let bits = u64::from(i & 0x07);
    let shift = (i >> 3) as i32 - 1;
    if shift == -1 {
        bits
    } else {
        (bits | 0x08) << shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn idf_hand_computed() {
        // N = 4, df = 2: ln(1 + 2.5 / 2.5) = ln 2
        assert!(close(idf(4, 2), std::f32::consts::LN_2));
        // N = 1000, df = 1: ln(1 + 999.5 / 1.5)
        assert!(close(idf(1000, 1), (1.0f64 + 999.5 / 1.5).ln() as f32));
        // A term in every document still gets a small *positive* weight.
        assert!(idf(10, 10) > 0.0);
    }

    #[test]
    fn idf_decreases_with_doc_freq() {
        let n = 10_000;
        let mut prev = f32::INFINITY;
        for df in [1, 10, 100, 1000, 5000, 10_000] {
            let v = idf(n, df);
            assert!(v < prev, "idf must strictly decrease, df={df}");
            prev = v;
        }
    }

    /// Hand-computed example (also used in `search` tests):
    /// 4 docs with lengths [3, 2, 0, 4] -> avgdl = 9/4 = 2.25. Term "rust" has
    /// df = 2 -> IDF = ln 2. With k1 = 1.2, b = 0.75:
    ///
    /// doc 0 (tf = 2, |D| = 3): norm = 1.2 * (0.25 + 0.75 * 3 / 2.25) = 1.5
    ///   score = ln2 * 2 * 2.2 / (2 + 1.5) = 0.693147 * 1.257143 = 0.871385
    /// doc 3 (tf = 1, |D| = 4): norm = 1.2 * (0.25 + 0.75 * 4 / 2.25) = 1.9
    ///   score = ln2 * 1 * 2.2 / (1 + 1.9) = 0.693147 * 0.758621 = 0.525836
    #[test]
    fn bm25_hand_computed() {
        let s = Bm25TermScorer::new(Bm25Params::CLASSIC, idf(4, 2), 1, 2.25);
        assert!(close(s.score(2, 3), 0.871385), "{}", s.score(2, 3));
        assert!(close(s.score(1, 4), 0.525836), "{}", s.score(1, 4));
    }

    #[test]
    fn query_tf_scales_linearly() {
        let one = Bm25TermScorer::new(Bm25Params::ANSERINI, 1.5, 1, 10.0);
        let three = Bm25TermScorer::new(Bm25Params::ANSERINI, 1.5, 3, 10.0);
        assert!(close(three.score(4, 12), 3.0 * one.score(4, 12)));
    }

    #[test]
    fn tf_saturates_towards_idf_times_k1_plus_1() {
        let p = Bm25Params::ANSERINI;
        let s = Bm25TermScorer::new(p, 1.0, 1, 10.0);
        let limit = p.k1 + 1.0;
        let mut prev = 0.0;
        for tf in [1, 2, 4, 8, 64, 10_000] {
            let v = s.score(tf, 10);
            assert!(v > prev && v < limit);
            prev = v;
        }
        assert!(limit - s.score(10_000, 10) < 1e-3);
    }

    #[test]
    fn b_zero_ignores_length_and_b_one_penalizes_long_docs() {
        let no_norm = Bm25Params {
            b: 0.0,
            ..Bm25Params::CLASSIC
        };
        let s = Bm25TermScorer::new(no_norm, 1.0, 1, 10.0);
        assert!(close(s.score(3, 1), s.score(3, 1000)));
        let s = Bm25TermScorer::new(Bm25Params::CLASSIC, 1.0, 1, 10.0);
        assert!(s.score(3, 5) > s.score(3, 50));
    }

    #[test]
    fn lucene_length_quantization_matches_smallfloat() {
        // Exact region.
        for len in 0..24 {
            assert_eq!(lucene_quantize_length(len), len);
        }
        // Expected values derived by hand from Lucene's SmallFloat source
        // (byte4ToInt(intToByte4(x)) = 24 + int4ToLong(longToInt4(x - 24))).
        // Lucene itself was not executed here, so these check our port
        // against the algorithm as written, not against a Lucene run.
        // x = 31 -> rest 7 (3 bits, subnormal)       -> 31
        // x = 32 -> rest 8 = 0b1000                   -> 32
        // x = 40 -> rest 16 = 0b10000, keep 4 bits -> 16 -> 40
        // x = 41 -> rest 17 = 0b10001 -> 0b1000 << 1 = 16 -> 40
        // x = 100 -> rest 76 = 0b1001100 -> 0b1001 << 3 = 72 -> 96
        assert_eq!(lucene_quantize_length(31), 31);
        assert_eq!(lucene_quantize_length(32), 32);
        assert_eq!(lucene_quantize_length(40), 40);
        assert_eq!(lucene_quantize_length(41), 40);
        assert_eq!(lucene_quantize_length(100), 96);
        // Never rounds up, and stays within 12.5% below.
        for len in 24..100_000u32 {
            let q = lucene_quantize_length(len);
            assert!(
                q <= len && (len - q) as f64 <= 0.125 * len as f64,
                "len {len} -> {q}"
            );
        }
    }
}
