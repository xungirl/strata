//! Text analysis: turning a string into the list of terms we index.
//!
//! The pipeline mirrors Lucene's `EnglishAnalyzer`, which is what Anserini
//! (the reference BM25 implementation we compare against) uses:
//!
//! ```text
//! "The Cat's running!"
//!   1. tokenize (Unicode UAX #29 words) -> ["The", "Cat's", "running"]
//!   2. strip English possessive "'s"    -> ["The", "Cat", "running"]
//!   3. lowercase                        -> ["the", "cat", "running"]
//!   4. remove stopwords                 -> ["cat", "running"]
//!   5. stem                             -> ["cat", "run"]
//! ```
//!
//! Every step is switchable through [`AnalyzerConfig`], which is `serde`
//! serializable: stage 2 writes it into each segment's metadata so that the
//! query side always analyzes text exactly like the index side did. A
//! mismatch there is a classic silent-bug source in search engines.
//!
//! Known differences from Lucene (documented in the README):
//! * Stemmer: `rust-stemmers` implements Snowball English ("Porter2"), while
//!   Lucene's `PorterStemFilter` implements the original 1980 Porter algorithm.
//!   They agree on most words but not all (e.g. Porter2 maps "generously" ->
//!   "generous", Porter -> "gener").
//! * Tokenizer: `unicode-segmentation` implements the same Unicode standard
//!   (UAX #29) as Lucene's `StandardTokenizer`, but edge cases (emoji,
//!   very long tokens, some scripts) may differ.

use rust_stemmers::{Algorithm, Stemmer};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;

/// Lucene's default English stopword list (`EnglishAnalyzer.ENGLISH_STOP_WORDS_SET`).
///
/// It is intentionally tiny (33 words). BM25's IDF already gives very common
/// words a near-zero weight, so the main benefit of stopwords is a smaller index
/// and faster queries, not better ranking. Using exactly Lucene's list keeps us
/// comparable to the published Anserini numbers.
pub const LUCENE_ENGLISH_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

/// Which stopword list to apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stopwords {
    /// Keep every token.
    None,
    /// [`LUCENE_ENGLISH_STOPWORDS`].
    LuceneEnglish,
    /// A caller-supplied list (matched after lowercasing).
    Custom(Vec<String>),
}

/// Which stemmer to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StemmerKind {
    /// No stemming: "running" stays "running".
    None,
    /// Snowball English, a.k.a. Porter2 (via the `rust-stemmers` crate).
    Porter2,
}

/// All knobs of the analysis pipeline. Plain data, so it can be stored,
/// compared and sent over the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzerConfig {
    /// Lowercase tokens (Unicode-aware, `str::to_lowercase`).
    pub lowercase: bool,
    /// Remove a trailing `'s` (also the typographic `’s`), like Lucene's
    /// `EnglishPossessiveFilter`.
    pub strip_possessive: bool,
    /// Stopword list applied after lowercasing.
    pub stopwords: Stopwords,
    /// Stemmer applied last.
    pub stemmer: StemmerKind,
    /// Tokens longer than this many bytes are dropped (Lucene's default
    /// maximum token length is 255 characters). Protects the index from
    /// garbage such as base64 blobs.
    pub max_token_bytes: usize,
}

impl AnalyzerConfig {
    /// The Lucene `EnglishAnalyzer`-like default used for all BEIR runs.
    pub fn english() -> Self {
        AnalyzerConfig {
            lowercase: true,
            strip_possessive: true,
            stopwords: Stopwords::LuceneEnglish,
            stemmer: StemmerKind::Porter2,
            max_token_bytes: 255,
        }
    }

    /// Tokenize + lowercase only. Useful in tests and as an ablation.
    pub fn simple() -> Self {
        AnalyzerConfig {
            lowercase: true,
            strip_possessive: false,
            stopwords: Stopwords::None,
            stemmer: StemmerKind::None,
            max_token_bytes: 255,
        }
    }
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        Self::english()
    }
}

/// A ready-to-use analyzer built from an [`AnalyzerConfig`].
///
/// Building it once (stopword `HashSet`, stemmer tables) and then reusing it for
/// every document avoids repeating that setup per call.
pub struct Analyzer {
    config: AnalyzerConfig,
    stopwords: HashSet<String>,
    // `Option<T>` is Rust's "maybe a value": `None` when stemming is off.
    stemmer: Option<Stemmer>,
}

impl Analyzer {
    /// Build an analyzer. Takes the config *by value* (ownership moves into the
    /// analyzer), so the analyzer can keep it and hand it back via [`config`](Self::config).
    pub fn new(config: AnalyzerConfig) -> Self {
        let stopwords: HashSet<String> = match &config.stopwords {
            Stopwords::None => HashSet::new(),
            Stopwords::LuceneEnglish => LUCENE_ENGLISH_STOPWORDS
                .iter()
                .map(|w| w.to_string())
                .collect(),
            Stopwords::Custom(words) => words.iter().map(|w| w.to_lowercase()).collect(),
        };
        let stemmer = match config.stemmer {
            StemmerKind::None => None,
            StemmerKind::Porter2 => Some(Stemmer::create(Algorithm::English)),
        };
        Analyzer {
            config,
            stopwords,
            stemmer,
        }
    }

    /// The configuration this analyzer was built from.
    pub fn config(&self) -> &AnalyzerConfig {
        &self.config
    }

    /// Analyze `text` into a list of owned terms, in document order.
    ///
    /// `&self` and `&str` are *borrows*: the function reads the analyzer and
    /// the text without taking ownership of either. It returns `String`s it
    /// owns because the stemmed/lowercased forms are new strings anyway.
    pub fn analyze(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.analyze_into(text, &mut out);
        out
    }

    /// Like [`analyze`](Self::analyze) but appends into a caller-provided
    /// buffer, so a hot loop can reuse one allocation.
    pub fn analyze_into(&self, text: &str, out: &mut Vec<String>) {
        // `unicode_words` yields the UAX #29 "words" that contain at least one
        // letter or digit, so punctuation and whitespace disappear here.
        for word in text.unicode_words() {
            if let Some(term) = self.normalize(word) {
                out.push(term);
            }
        }
    }

    /// Steps 2-5 of the pipeline for a single token. Returns `None` when the
    /// token is filtered out (stopword or too long).
    fn normalize(&self, word: &str) -> Option<String> {
        if word.len() > self.config.max_token_bytes {
            return None;
        }
        let word = if self.config.strip_possessive {
            strip_possessive(word)
        } else {
            word
        };
        if word.is_empty() {
            return None;
        }
        let lowered = if self.config.lowercase {
            word.to_lowercase()
        } else {
            word.to_string()
        };
        if self.stopwords.contains(&lowered) {
            return None;
        }
        match &self.stemmer {
            // `stem` returns a `Cow<str>`: borrowed if unchanged, owned if
            // changed. `into_owned` gives us a `String` either way.
            Some(stemmer) => Some(stemmer.stem(&lowered).into_owned()),
            None => Some(lowered),
        }
    }
}

/// Remove a trailing English possessive (`'s`, `'S`, `’s`, `＇s`).
///
/// Returns a sub-slice of the input: no allocation, the result borrows from
/// `word`. The full signature is `fn<'a>(word: &'a str) -> &'a str` ("the
/// output lives as long as the input"); Rust's *lifetime elision* rules let us
/// omit `'a` because there is exactly one input reference.
fn strip_possessive(word: &str) -> &str {
    // Walk backwards over the last two characters. `char_indices` gives byte
    // offsets, which we need because Rust strings are UTF-8 and the curly
    // apostrophe takes 3 bytes.
    let mut rev = word.char_indices().rev();
    match (rev.next(), rev.next()) {
        (Some((_, 's' | 'S')), Some((apos_start, '\'' | '\u{2019}' | '\u{FF07}'))) => {
            &word[..apos_start]
        }
        _ => word,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english(text: &str) -> Vec<String> {
        Analyzer::new(AnalyzerConfig::english()).analyze(text)
    }

    #[test]
    fn full_pipeline_example_from_module_docs() {
        assert_eq!(english("The Cat's running!"), vec!["cat", "run"]);
    }

    #[test]
    fn punctuation_and_whitespace_are_separators() {
        assert_eq!(
            Analyzer::new(AnalyzerConfig::simple()).analyze("  hello,world\t(foo) -- bar.\n"),
            vec!["hello", "world", "foo", "bar"]
        );
    }

    #[test]
    fn numbers_and_decimals_stay_whole() {
        // UAX #29 keeps "3.14" and "1,000" as single tokens, like Lucene.
        assert_eq!(
            Analyzer::new(AnalyzerConfig::simple()).analyze("pi is 3.14 not 1,000"),
            vec!["pi", "is", "3.14", "not", "1,000"]
        );
    }

    #[test]
    fn unicode_lowercasing_and_non_latin_scripts() {
        let a = Analyzer::new(AnalyzerConfig::simple());
        assert_eq!(
            a.analyze("ÉCOLE Straße ΑΘΗΝΑ"),
            vec!["école", "straße", "αθηνα"]
        );
        // Ideographs are split one per token by UAX #29.
        assert_eq!(a.analyze("搜索引擎"), vec!["搜", "索", "引", "擎"]);
    }

    #[test]
    fn stopwords_removed_only_when_enabled() {
        assert_eq!(english("this is a test"), vec!["test"]);
        let simple = Analyzer::new(AnalyzerConfig::simple());
        assert_eq!(simple.analyze("this is a"), vec!["this", "is", "a"]);
    }

    #[test]
    fn custom_stopwords_are_case_insensitive() {
        let cfg = AnalyzerConfig {
            stopwords: Stopwords::Custom(vec!["FOO".into()]),
            ..AnalyzerConfig::simple()
        };
        assert_eq!(Analyzer::new(cfg).analyze("foo Foo bar"), vec!["bar"]);
    }

    #[test]
    fn possessive_variants() {
        assert_eq!(strip_possessive("john's"), "john");
        assert_eq!(strip_possessive("JOHN'S"), "JOHN");
        assert_eq!(strip_possessive("john\u{2019}s"), "john");
        assert_eq!(strip_possessive("johns"), "johns");
        assert_eq!(strip_possessive("'s"), "");
    }

    #[test]
    fn porter2_stems() {
        assert_eq!(
            english("connected connecting connection connections"),
            vec!["connect", "connect", "connect", "connect"]
        );
        assert_eq!(english("studies studying"), vec!["studi", "studi"]);
    }

    #[test]
    fn overly_long_tokens_are_dropped() {
        let cfg = AnalyzerConfig {
            max_token_bytes: 5,
            ..AnalyzerConfig::simple()
        };
        assert_eq!(
            Analyzer::new(cfg).analyze("short loooooong ok"),
            vec!["short", "ok"]
        );
    }

    #[test]
    fn empty_and_symbol_only_input() {
        assert!(english("").is_empty());
        assert!(english("!!! ... ---").is_empty());
    }

    #[test]
    fn config_roundtrips_through_json() {
        let cfg = AnalyzerConfig::english();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AnalyzerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }
}
