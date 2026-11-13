//! A small Lucene-flavoured query language.
//!
//! ```text
//! query  := item*
//! item   := ['+' | '-'] ( '"' words '"'          phrase
//!                       | field ':' value          keyword / int equality filter
//!                       | field ':[' lo ' TO ' hi ']'  inclusive int range filter ('*' = open)
//!                       | word '*'                 prefix
//!                       | word )                   term
//! ```
//!
//! Examples: `covid vaccine`, `+vaccine -mrna "spike protein"`,
//! `vacc* year:[2019 TO 2021] -lang:de`.
//!
//! * Plain words and phrases are analyzed with the index's analyzer, so
//!   `Running` matches `run`. A word that analyzes to nothing (a stopword)
//!   is dropped; a word that analyzes to several tokens (`e-mail`) becomes
//!   several term clauses with the same occurrence.
//! * Prefixes are only lowercased (not stemmed), like Lucene wildcards.
//! * Filters never affect scores. `+field:...` and `field:...` both mean
//!   "required"; `-field:...` negates the filter.
//! * Repeated plain words count multiple times (`qtf`), exactly like the
//!   bag-of-words queries used for BEIR, so `strata-eval` can send raw BEIR
//!   query text through this parser and get the same BM25 scores.
//! * No parentheses or `AND`/`OR` keywords (yet); words are literal.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use strata_core::analyzer::Analyzer;

use crate::ast::{BoolQuery, Clause, Filter};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Occur {
    Must,
    Should,
    MustNot,
}

/// Split the input into raw items, keeping quoted phrases and range
/// brackets together.
fn lex(input: &str) -> Result<Vec<String>> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut in_range = false;
    for c in input.chars() {
        match c {
            '"' => {
                cur.push(c);
                in_quote = !in_quote;
            }
            '[' if !in_quote => {
                cur.push(c);
                in_range = true;
            }
            ']' if !in_quote => {
                cur.push(c);
                in_range = false;
            }
            c if c.is_whitespace() && !in_quote && !in_range => {
                if !cur.is_empty() {
                    items.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if in_quote {
        bail!("unterminated quote in query");
    }
    if in_range {
        bail!("unterminated range in query");
    }
    if !cur.is_empty() {
        items.push(cur);
    }
    Ok(items)
}

fn is_field_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_bound(s: &str) -> Result<Option<i64>> {
    let s = s.trim();
    if s == "*" {
        return Ok(None);
    }
    match s.parse::<i64>() {
        Ok(v) => Ok(Some(v)),
        Err(_) => bail!("range bound {s:?} is not an integer"),
    }
}

fn parse_filter(field: &str, value: &str) -> Result<Filter> {
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let Some((lo, hi)) = inner.split_once(" TO ") else {
            bail!("range must look like [lo TO hi], got {value:?}");
        };
        return Ok(Filter::IntRange {
            field: field.to_string(),
            lo: parse_bound(lo)?,
            hi: parse_bound(hi)?,
        });
    }
    if value.is_empty() {
        bail!("empty value for field {field:?}");
    }
    Ok(match value.parse::<i64>() {
        Ok(v) => Filter::IntRange {
            field: field.to_string(),
            lo: Some(v),
            hi: Some(v),
        },
        Err(_) => Filter::KeywordEq {
            field: field.to_string(),
            value: value.to_string(),
        },
    })
}

/// Parse `input` into a [`BoolQuery`], analyzing words with `analyzer`.
pub fn parse(input: &str, analyzer: &Analyzer) -> Result<BoolQuery> {
    let mut q = BoolQuery::default();
    // Plain terms per occurrence, grouped so repeats become counts.
    let mut terms: [BTreeMap<String, u32>; 3] = Default::default();
    for item in lex(input)? {
        let (occur, body) = match item.strip_prefix('+') {
            Some(rest) => (Occur::Must, rest),
            None => match item.strip_prefix('-') {
                Some(rest) => (Occur::MustNot, rest),
                None => (Occur::Should, item.as_str()),
            },
        };
        if body.is_empty() {
            continue;
        }
        let slot = occur as usize;
        if let Some(phrase) = body.strip_prefix('"').and_then(|b| b.strip_suffix('"')) {
            let tokens = analyzer.analyze(phrase);
            let clause = match tokens.len() {
                0 => continue,
                1 => {
                    *terms[slot].entry(tokens[0].clone()).or_default() += 1;
                    continue;
                }
                _ => Clause::Phrase(tokens),
            };
            push(&mut q, occur, clause);
        } else if let Some((field, value)) = body.split_once(':').filter(|(f, _)| is_field_name(f))
        {
            let f = parse_filter(field, value)?;
            q.filters.push(if occur == Occur::MustNot {
                Filter::Not(Box::new(f))
            } else {
                f
            });
        } else if let Some(prefix) = body.strip_suffix('*').filter(|p| !p.is_empty()) {
            push(&mut q, occur, Clause::Prefix(prefix.to_lowercase()));
        } else {
            for t in analyzer.analyze(body) {
                *terms[slot].entry(t).or_default() += 1;
            }
        }
    }
    // Term clauses first (sorted), then phrases/prefixes in input order.
    for (slot, occur) in [(0, Occur::Must), (1, Occur::Should), (2, Occur::MustNot)] {
        let mut clauses: Vec<Clause> = std::mem::take(&mut terms[slot])
            .into_iter()
            .map(|(term, count)| Clause::Term { term, count })
            .collect();
        let list = match occur {
            Occur::Must => &mut q.must,
            Occur::Should => &mut q.should,
            Occur::MustNot => &mut q.must_not,
        };
        clauses.append(list);
        *list = clauses;
    }
    Ok(q)
}

fn push(q: &mut BoolQuery, occur: Occur, c: Clause) {
    match occur {
        Occur::Must => q.must.push(c),
        Occur::Should => q.should.push(c),
        Occur::MustNot => q.must_not.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::analyzer::AnalyzerConfig;

    fn p(s: &str) -> BoolQuery {
        parse(s, &Analyzer::new(AnalyzerConfig::english())).unwrap()
    }

    fn term(t: &str, c: u32) -> Clause {
        Clause::Term {
            term: t.into(),
            count: c,
        }
    }

    #[test]
    fn plain_words_are_analyzed_and_counted() {
        let q = p("Running the dogs running");
        assert_eq!(q.should, vec![term("dog", 1), term("run", 2)]);
        assert_eq!(
            q.as_disjunction(),
            Some(vec![("dog".into(), 1), ("run".into(), 2)])
        );
    }

    #[test]
    fn occurrences_phrases_prefixes() {
        let q = p(r#"+vaccine -mRNA "spike proteins" covi*"#);
        assert_eq!(q.must, vec![term("vaccin", 1)]);
        assert_eq!(q.must_not, vec![term("mrna", 1)]);
        assert_eq!(
            q.should,
            vec![
                Clause::Phrase(vec!["spike".into(), "protein".into()]),
                Clause::Prefix("covi".into())
            ]
        );
        assert_eq!(q.as_disjunction(), None);
    }

    #[test]
    fn filters() {
        let q = p("cancer year:[2019 TO *] -lang:de bucket:7");
        assert_eq!(
            q.filters,
            vec![
                Filter::IntRange {
                    field: "year".into(),
                    lo: Some(2019),
                    hi: None
                },
                Filter::Not(Box::new(Filter::KeywordEq {
                    field: "lang".into(),
                    value: "de".into()
                })),
                Filter::IntRange {
                    field: "bucket".into(),
                    lo: Some(7),
                    hi: Some(7)
                },
            ]
        );
        // Filters don't stop a term disjunction from using WAND.
        assert_eq!(q.as_disjunction(), Some(vec![("cancer".into(), 1)]));
    }

    #[test]
    fn single_word_phrase_is_a_term_and_stopword_phrase_vanishes() {
        assert_eq!(p(r#""Cats""#).should, vec![term("cat", 1)]);
        assert!(p(r#""the of""#).should.is_empty());
    }

    #[test]
    fn colon_in_ordinary_text_is_not_a_filter() {
        // "12:30" has no valid field name before the colon.
        let q = p("meet at 12:30");
        assert!(q.filters.is_empty());
    }

    #[test]
    fn errors() {
        let a = Analyzer::new(AnalyzerConfig::english());
        assert!(parse(r#""open phrase"#, &a).is_err());
        assert!(parse("year:[2019 TO", &a).is_err());
        assert!(parse("year:[a TO b]", &a).is_err());
        assert!(parse("year:[2019 2020]", &a).is_err());
    }
}
