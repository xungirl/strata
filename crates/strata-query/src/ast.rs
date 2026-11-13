//! The query AST produced by the parser and consumed by the executor.
//!
//! Semantics follow Lucene's `BooleanQuery` (without coord factors):
//! * `must` clauses: every one has to match; they contribute to the score;
//! * `should` clauses: optional when there is a `must`, otherwise at least
//!   one has to match; matching ones contribute to the score;
//! * `must_not` clauses: matching docs are excluded; no score;
//! * filters: required, non-scoring conditions on metadata fields.

/// A scoring clause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clause {
    /// An analyzed term with its multiplicity in the query (`qtf`).
    Term { term: String, count: u32 },
    /// Consecutive analyzed terms (positions p, p+1, ...).
    Phrase(Vec<String>),
    /// All terms starting with this (lowercased, unstemmed) prefix;
    /// constant score 1.0 per matching doc, like Lucene's default rewrite.
    Prefix(String),
}

/// A non-scoring condition on a metadata field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// `field:value` on a keyword field.
    KeywordEq { field: String, value: String },
    /// `field:[lo TO hi]` on an int field, inclusive; `None` = unbounded.
    IntRange {
        field: String,
        lo: Option<i64>,
        hi: Option<i64>,
    },
    /// `-field:...`: docs that do *not* satisfy the inner filter.
    Not(Box<Filter>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoolQuery {
    pub must: Vec<Clause>,
    pub should: Vec<Clause>,
    pub must_not: Vec<Clause>,
    pub filters: Vec<Filter>,
}

impl BoolQuery {
    /// If this is a plain disjunction of terms (optionally filtered), the
    /// terms with counts, sorted. Such queries can use block-max WAND.
    pub fn as_disjunction(&self) -> Option<Vec<(String, u32)>> {
        if !self.must.is_empty() || !self.must_not.is_empty() || self.should.is_empty() {
            return None;
        }
        let mut terms = Vec::new();
        for c in &self.should {
            match c {
                Clause::Term { term, count } => terms.push((term.clone(), *count)),
                _ => return None,
            }
        }
        terms.sort();
        Some(terms)
    }
}
