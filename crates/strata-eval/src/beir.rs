//! Loader for BEIR datasets (<https://github.com/beir-cellar/beir>).
//!
//! On-disk layout (after `scripts/download_beir.sh`):
//!
//! ```text
//! data/<name>/corpus.jsonl    {"_id": "...", "title": "...", "text": "..."}  one per line
//! data/<name>/queries.jsonl   {"_id": "...", "text": "..."}
//! data/<name>/qrels/test.tsv  query-id <TAB> corpus-id <TAB> score   (first line is a header)
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::metrics::Qrels;

/// One corpus document.
#[derive(Clone, Debug, Deserialize)]
pub struct Document {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
}

impl Document {
    /// The indexed text: title and body concatenated, the "flat" setup used by
    /// Anserini's BEIR regressions (`BeirFlatCollection`).
    pub fn flat_text(&self) -> String {
        if self.title.is_empty() {
            self.text.clone()
        } else {
            format!("{} {}", self.title, self.text)
        }
    }
}

#[derive(Deserialize)]
struct RawQuery {
    #[serde(rename = "_id")]
    id: String,
    text: String,
}

/// A loaded dataset: corpus, the queries that have judgments in the split,
/// and the judgments.
pub struct Dataset {
    pub name: String,
    pub corpus: Vec<Document>,
    /// `(query id, query text)`, sorted by query id for deterministic runs.
    pub queries: Vec<(String, String)>,
    pub qrels: Qrels,
}

impl Dataset {
    /// Load `<data_dir>/<name>` for the given qrels `split` (e.g. `"test"`).
    ///
    /// Returns `Result<Dataset>`: either the dataset or an error with context
    /// explaining which file failed. The `?` operator returns early with the
    /// error if a step fails, like a checked exception that is part of the
    /// type signature.
    pub fn load(data_dir: &Path, name: &str, split: &str) -> Result<Dataset> {
        let dir = data_dir.join(name);
        if !dir.exists() {
            bail!(
                "dataset directory {} not found; run scripts/download_beir.sh {name}",
                dir.display()
            );
        }
        let corpus = read_jsonl::<Document>(&dir.join("corpus.jsonl"))?;
        let qrels = read_qrels(&dir.join("qrels").join(format!("{split}.tsv")))?;

        let mut queries: Vec<(String, String)> =
            read_jsonl::<RawQuery>(&dir.join("queries.jsonl"))?
                .into_iter()
                .filter(|q| qrels.contains_key(&q.id))
                .map(|q| (q.id, q.text))
                .collect();
        queries.sort();
        if queries.len() != qrels.len() {
            bail!(
                "{name}: {} qrels queries but only {} found in queries.jsonl",
                qrels.len(),
                queries.len()
            );
        }
        Ok(Dataset {
            name: name.to_string(),
            corpus,
            queries,
            qrels,
        })
    }
}

/// Parse a JSON-lines file into a `Vec<T>` for any deserializable `T`.
/// `T: for<'de> Deserialize<'de>` is spelled `serde::de::DeserializeOwned`.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let item = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: bad JSON", path.display(), i + 1))?;
        out.push(item);
    }
    Ok(out)
}

/// Parse a BEIR qrels TSV (with header) into [`Qrels`].
pub fn read_qrels(path: &Path) -> Result<Qrels> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut qrels: Qrels = HashMap::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if i == 0 && line.starts_with("query-id") {
            continue; // header
        }
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [qid, docid, rel] = cols[..] else {
            bail!(
                "{}:{}: expected 3 tab-separated columns",
                path.display(),
                i + 1
            );
        };
        let rel: i32 = rel
            .trim()
            .parse()
            .with_context(|| format!("{}:{}: bad relevance {rel:?}", path.display(), i + 1))?;
        qrels
            .entry(qid.to_string())
            .or_default()
            .insert(docid.to_string(), rel);
    }
    Ok(qrels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A throwaway directory under the OS temp dir, removed on drop
    /// (the `Drop` trait is Rust's destructor).
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(path)
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();
    }

    #[test]
    fn loads_tiny_dataset() {
        let root = std::env::temp_dir().join(format!("strata-beir-test-{}", std::process::id()));
        let _guard = TempDir(root.clone());
        let d = root.join("toy");
        write(
            &d.join("corpus.jsonl"),
            "{\"_id\":\"d1\",\"title\":\"Hello\",\"text\":\"world\"}\n{\"_id\":\"d2\",\"text\":\"no title\"}\n",
        );
        write(
            &d.join("queries.jsonl"),
            "{\"_id\":\"q1\",\"text\":\"hello\"}\n{\"_id\":\"q9\",\"text\":\"unjudged\"}\n",
        );
        write(
            &d.join("qrels/test.tsv"),
            "query-id\tcorpus-id\tscore\nq1\td1\t2\nq1\td2\t0\n",
        );

        let ds = Dataset::load(&root, "toy", "test").unwrap();
        assert_eq!(ds.corpus.len(), 2);
        assert_eq!(ds.corpus[0].flat_text(), "Hello world");
        assert_eq!(ds.corpus[1].flat_text(), "no title");
        // Only judged queries are kept.
        assert_eq!(ds.queries, vec![("q1".to_string(), "hello".to_string())]);
        assert_eq!(ds.qrels["q1"]["d1"], 2);
        assert_eq!(ds.qrels["q1"]["d2"], 0);
    }

    #[test]
    fn missing_dataset_is_a_helpful_error() {
        let err = Dataset::load(Path::new("/nonexistent"), "nope", "test")
            .err()
            .unwrap();
        assert!(err.to_string().contains("download_beir.sh"));
    }
}
