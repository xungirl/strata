//! Per-document metadata fields ("doc values"), stored column-wise.
//!
//! Search filters such as `year:[2019 TO 2021]` or `lang:en` need to read
//! one value per candidate document quickly. Storing each field as a column
//! (one fixed-width slot per doc) makes that an array lookup:
//!
//! ```text
//! fields.json   schema: [{name, kind, offset, values (keyword dictionary)}]
//! fields.bin    int column:     num_docs * i64 LE   (i64::MIN = missing)
//!               keyword column: num_docs * u32 LE   (ordinal into the sorted
//!                                                    dictionary; u32::MAX = missing)
//! ```
//!
//! Keyword dictionaries live in `fields.json`, which is fine for the
//! low-cardinality fields filters are typically used on (language, source,
//! category). High-cardinality string fields would need an FST like terms.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::codec::{read_u32, read_u64};
use crate::segment::{map_file, Bytes};

/// A field value attached to a document.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FieldValue {
    Keyword(String),
    Int(i64),
}

impl FieldValue {
    pub fn kind(&self) -> FieldKind {
        match self {
            FieldValue::Keyword(_) => FieldKind::Keyword,
            FieldValue::Int(_) => FieldKind::Int,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Keyword,
    Int,
}

/// A document's fields: name -> value (at most one value per field).
pub type Fields = BTreeMap<String, FieldValue>;

const MISSING_INT: i64 = i64::MIN;
const MISSING_ORD: u32 = u32::MAX;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FieldSchema {
    name: String,
    kind: FieldKind,
    /// Byte offset of the column in fields.bin.
    offset: u64,
    /// Sorted keyword dictionary (empty for ints).
    #[serde(default)]
    values: Vec<String>,
}

/// Write `fields.json` + `fields.bin` for `docs` (one `Fields` per doc).
/// Fails if a field name is used with two different kinds.
pub fn write_fields(dir: &Path, docs: &[Fields]) -> Result<()> {
    // Schema: every field name seen, with its kind and keyword dictionary.
    let mut kinds: BTreeMap<&str, FieldKind> = BTreeMap::new();
    let mut dicts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for doc in docs {
        for (name, v) in doc {
            match kinds.insert(name, v.kind()) {
                Some(k) if k != v.kind() => {
                    bail!("field {name:?} used as both {k:?} and {:?}", v.kind())
                }
                _ => {}
            }
            if let FieldValue::Keyword(s) = v {
                dicts.entry(name).or_default().insert(s);
            }
        }
    }
    let mut bin = Vec::new();
    let mut schema = Vec::new();
    for (&name, &kind) in &kinds {
        let offset = bin.len() as u64;
        match kind {
            FieldKind::Int => {
                for doc in docs {
                    let v = match doc.get(name) {
                        Some(FieldValue::Int(i)) => *i,
                        _ => MISSING_INT,
                    };
                    bin.extend_from_slice(&v.to_le_bytes());
                }
                schema.push(FieldSchema {
                    name: name.to_string(),
                    kind,
                    offset,
                    values: Vec::new(),
                });
            }
            FieldKind::Keyword => {
                let values: Vec<&str> = dicts[name].iter().copied().collect();
                for doc in docs {
                    let ord = match doc.get(name) {
                        Some(FieldValue::Keyword(s)) => {
                            values.binary_search(&s.as_str()).expect("in dict") as u32
                        }
                        _ => MISSING_ORD,
                    };
                    bin.extend_from_slice(&ord.to_le_bytes());
                }
                schema.push(FieldSchema {
                    name: name.to_string(),
                    kind,
                    offset,
                    values: values.iter().map(|s| s.to_string()).collect(),
                });
            }
        }
    }
    fs::write(dir.join("fields.json"), serde_json::to_vec(&schema)?)?;
    fs::write(dir.join("fields.bin"), bin)?;
    Ok(())
}

/// Read-only access to a segment's field columns.
pub struct FieldColumns {
    schema: Vec<FieldSchema>,
    bin: Bytes,
}

/// One column of one segment.
#[derive(Clone, Copy)]
pub struct Column<'a> {
    schema: &'a FieldSchema,
    bin: &'a [u8],
}

impl FieldColumns {
    pub fn open(dir: &Path) -> Result<FieldColumns> {
        Ok(FieldColumns {
            schema: serde_json::from_slice(&fs::read(dir.join("fields.json"))?)?,
            bin: map_file(&dir.join("fields.bin"))?,
        })
    }

    /// (name, kind) of every field present in this segment.
    pub fn kinds(&self) -> impl Iterator<Item = (&str, FieldKind)> + '_ {
        self.schema.iter().map(|s| (s.name.as_str(), s.kind))
    }

    pub fn column(&self, name: &str) -> Option<Column<'_>> {
        self.schema
            .iter()
            .find(|s| s.name == name)
            .map(|schema| Column {
                schema,
                bin: &self.bin,
            })
    }

    /// All field values of `doc` (used when merging segments).
    pub fn doc_fields(&self, doc: u32) -> Fields {
        let mut out = Fields::new();
        for schema in &self.schema {
            let c = Column {
                schema,
                bin: &self.bin,
            };
            if let Some(v) = c.get(doc) {
                out.insert(schema.name.clone(), v);
            }
        }
        out
    }
}

impl<'a> Column<'a> {
    pub fn kind(&self) -> FieldKind {
        self.schema.kind
    }

    /// Integer value of `doc`, if this is an int column and the doc has one.
    #[inline]
    pub fn int(&self, doc: u32) -> Option<i64> {
        if self.schema.kind != FieldKind::Int {
            return None;
        }
        let v = read_u64(self.bin, self.schema.offset as usize + doc as usize * 8) as i64;
        (v != MISSING_INT).then_some(v)
    }

    /// Keyword ordinal of `doc` (index into the sorted dictionary).
    #[inline]
    pub fn ord(&self, doc: u32) -> Option<u32> {
        if self.schema.kind != FieldKind::Keyword {
            return None;
        }
        let o = read_u32(self.bin, self.schema.offset as usize + doc as usize * 4);
        (o != MISSING_ORD).then_some(o)
    }

    /// Ordinal of a keyword value in this segment's dictionary.
    pub fn ord_of(&self, value: &str) -> Option<u32> {
        self.schema
            .values
            .binary_search_by(|v| v.as_str().cmp(value))
            .ok()
            .map(|i| i as u32)
    }

    pub fn get(&self, doc: u32) -> Option<FieldValue> {
        match self.schema.kind {
            FieldKind::Int => self.int(doc).map(FieldValue::Int),
            FieldKind::Keyword => self
                .ord(doc)
                .map(|o| FieldValue::Keyword(self.schema.values[o as usize].clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_columns() {
        let dir = std::env::temp_dir().join(format!("strata-fields-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let docs: Vec<Fields> = vec![
            [
                ("year".to_string(), FieldValue::Int(2020)),
                ("lang".to_string(), FieldValue::Keyword("en".into())),
            ]
            .into(),
            Fields::new(),
            [
                ("year".to_string(), FieldValue::Int(-5)),
                ("lang".to_string(), FieldValue::Keyword("de".into())),
            ]
            .into(),
        ];
        write_fields(&dir, &docs).unwrap();
        let cols = FieldColumns::open(&dir).unwrap();
        let year = cols.column("year").unwrap();
        assert_eq!(
            (year.int(0), year.int(1), year.int(2)),
            (Some(2020), None, Some(-5))
        );
        let lang = cols.column("lang").unwrap();
        assert_eq!(lang.ord_of("de"), Some(0)); // dictionary is sorted
        assert_eq!(lang.ord(0), lang.ord_of("en"));
        assert_eq!(lang.ord(1), None);
        assert!(cols.column("nope").is_none());
        for (d, want) in docs.iter().enumerate() {
            assert_eq!(&cols.doc_fields(d as u32), want);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conflicting_kinds_rejected() {
        let dir = std::env::temp_dir().join(format!("strata-fields-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let docs: Vec<Fields> = vec![
            [("x".to_string(), FieldValue::Int(1))].into(),
            [("x".to_string(), FieldValue::Keyword("a".into()))].into(),
        ];
        assert!(write_fields(&dir, &docs).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
