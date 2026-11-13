//! Compile [`Filter`]s against one segment's field columns into a fast
//! per-document predicate (combined with the segment's tombstones).
//!
//! Filters are evaluated lazily, only for candidate documents, which costs
//! nothing when there are no filters and stays proportional to the number
//! of candidates otherwise.

use strata_storage::fields::{Column, FieldKind};
use strata_storage::snapshot::SnapshotSegment;

use crate::ast::Filter;

enum Cond<'a> {
    /// Keyword equality, already resolved to this segment's ordinal
    /// (`None`: value absent from the segment, nothing matches).
    Ord(Option<Column<'a>>, Option<u32>),
    IntRange(Option<Column<'a>>, i64, i64),
    Not(Box<Cond<'a>>),
}

impl Cond<'_> {
    #[inline]
    fn eval(&self, doc: u32) -> bool {
        match self {
            Cond::Ord(col, ord) => match (col, ord) {
                (Some(c), Some(o)) => c.ord(doc) == Some(*o),
                _ => false,
            },
            Cond::IntRange(col, lo, hi) => col
                .and_then(|c| c.int(doc))
                .is_some_and(|v| *lo <= v && v <= *hi),
            Cond::Not(inner) => !inner.eval(doc),
        }
    }
}

/// "May this doc be returned?" for one segment: live and passes all filters.
pub struct SegmentFilter<'a> {
    seg: &'a SnapshotSegment,
    conds: Vec<Cond<'a>>,
}

fn compile<'a>(seg: &'a SnapshotSegment, f: &Filter) -> Cond<'a> {
    let fields = seg.segment.fields();
    match f {
        Filter::KeywordEq { field, value } => {
            let col = fields
                .column(field)
                .filter(|c| c.kind() == FieldKind::Keyword);
            let ord = col.and_then(|c| c.ord_of(value));
            Cond::Ord(col, ord)
        }
        Filter::IntRange { field, lo, hi } => {
            let col = fields.column(field).filter(|c| c.kind() == FieldKind::Int);
            Cond::IntRange(col, lo.unwrap_or(i64::MIN + 1), hi.unwrap_or(i64::MAX))
        }
        Filter::Not(inner) => Cond::Not(Box::new(compile(seg, inner))),
    }
}

impl<'a> SegmentFilter<'a> {
    pub fn new(seg: &'a SnapshotSegment, filters: &[Filter]) -> SegmentFilter<'a> {
        SegmentFilter {
            seg,
            conds: filters.iter().map(|f| compile(seg, f)).collect(),
        }
    }

    #[inline]
    pub fn allows(&self, doc: u32) -> bool {
        self.seg.live.is_live(doc) && self.conds.iter().all(|c| c.eval(doc))
    }
}
