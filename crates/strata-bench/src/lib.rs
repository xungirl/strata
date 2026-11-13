//! # strata-bench
//!
//! Reference baselines for strata. Tantivy (a mature Rust search library,
//! Lucene-like) is used here **only as a yardstick**; the strata engine itself
//! never depends on it.
//!
//! To make the comparison about *indexing and ranking* rather than about text
//! analysis, [`StrataTokenizer`] plugs strata's own [`Analyzer`] into Tantivy,
//! so both engines index exactly the same terms.

use std::sync::Arc;

use strata_core::Analyzer;
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Tantivy tokenizer that delegates to a strata [`Analyzer`].
///
/// `Arc` (atomically reference-counted pointer) lets Tantivy clone the
/// tokenizer cheaply for each indexing thread while all clones share one
/// analyzer.
#[derive(Clone)]
pub struct StrataTokenizer {
    analyzer: Arc<Analyzer>,
}

impl StrataTokenizer {
    pub fn new(analyzer: Analyzer) -> Self {
        StrataTokenizer {
            analyzer: Arc::new(analyzer),
        }
    }
}

/// Pre-analyzed tokens handed to Tantivy one at a time.
pub struct StrataTokenStream {
    tokens: std::vec::IntoIter<String>,
    current: Token,
    position: usize,
}

impl Tokenizer for StrataTokenizer {
    type TokenStream<'a> = StrataTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        StrataTokenStream {
            tokens: self.analyzer.analyze(text).into_iter(),
            current: Token::default(),
            position: 0,
        }
    }
}

impl TokenStream for StrataTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(text) => {
                // Offsets are not needed for BM25 (no highlighting), so we only
                // fill in the text and a running position.
                self.current = Token {
                    text,
                    position: self.position,
                    position_length: 1,
                    ..Token::default()
                };
                self.position += 1;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::AnalyzerConfig;

    #[test]
    fn emits_the_same_terms_as_strata() {
        let mut tok = StrataTokenizer::new(Analyzer::new(AnalyzerConfig::english()));
        let mut stream = tok.token_stream("The Cat's running quickly");
        let mut got = Vec::new();
        while stream.advance() {
            got.push((stream.token().text.clone(), stream.token().position));
        }
        assert_eq!(
            got,
            vec![
                ("cat".to_string(), 0),
                ("run".to_string(), 1),
                ("quick".to_string(), 2)
            ]
        );
    }
}
