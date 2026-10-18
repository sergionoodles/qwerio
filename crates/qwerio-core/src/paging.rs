//! Bounded table pages with opaque continuation tokens.
//!
//! Keyset paging is used when a deterministic order exists: the user's sort
//! keys followed by a unique, non-NULL tie-breaker (primary key, unique key, or
//! hidden row locator). Otherwise pages use bounded OFFSET paging, which can
//! skip or repeat rows when the data changes between pages.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::catalog::ObjectDetails;
use crate::error::{DatabaseError, DbResult, ErrorKind};
use crate::filter::{
    Filter, FilterOperand, NullsOrder, SortDirection, SortKey, filters_to_predicate,
};
use crate::result::{ColumnDescriptor, Row, estimated_row_bytes};
use crate::sql::{
    ColumnRef, CompareOp, OBJECT_ALIAS, OrderTerm, Predicate, RenderedSql, SqlDialect, SqlRenderer,
};
use crate::value::Value;

/// Offset paging stops after this many rows; keyset paging has no limit.
pub const MAX_OFFSET_ROWS: u64 = 100_000;

pub const DEFAULT_PAGE_SIZE: usize = 200;

/// Maximum retained bytes for one page.
pub const DEFAULT_PAGE_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PageRequest {
    pub details: Arc<ObjectDetails>,
    pub filters: Vec<Filter>,
    pub sort: Vec<SortKey>,
    pub page_size: usize,
    pub max_bytes: usize,
    pub continuation: Option<ContinuationToken>,
}

impl PageRequest {
    pub fn first(details: Arc<ObjectDetails>) -> Self {
        Self {
            details,
            filters: Vec::new(),
            sort: Vec::new(),
            page_size: DEFAULT_PAGE_SIZE,
            max_bytes: DEFAULT_PAGE_MAX_BYTES,
            continuation: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingMode {
    /// Stable continuation from the last row's ordering key.
    Keyset,
    /// Continuation by row count; concurrent changes may shift rows.
    Offset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageNotice {
    /// The page was cut short to stay within its byte budget.
    ByteLimit,
    /// Offset paging reached [`MAX_OFFSET_ROWS`]; refine filters to see more.
    OffsetLimitReached,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub columns: Vec<ColumnDescriptor>,
    pub rows: Vec<Row>,
    pub next: Option<ContinuationToken>,
    pub mode: PagingMode,
    pub notice: Option<PageNotice>,
}

/// An opaque position in a specific query. It is only valid for the same
/// object, metadata revision, filters, and sort that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContinuationToken {
    fingerprint: u64,
    position: Position,
}

#[derive(Clone, Debug, PartialEq)]
enum Position {
    Keyset(Vec<Value>),
    Offset(u64),
}

/// A planned page query. Adapters render it, execute it, and pass the raw rows
/// (data columns followed by key columns) back to [`PagePlan::build_page`].
#[derive(Clone, Debug)]
pub struct PagePlan {
    details: Arc<ObjectDetails>,
    fingerprint: u64,
    mode: PagingMode,
    order: Vec<OrderTerm>,
    predicate: Predicate,
    page_size: usize,
    max_bytes: usize,
    offset: u64,
}

/// A rendered page query.
#[derive(Clone, Debug, PartialEq)]
pub struct PageQuery {
    pub sql: RenderedSql,
    /// Number of leading data columns; the rest are ordering keys.
    pub data_columns: usize,
}

impl PagePlan {
    pub fn new(request: &PageRequest) -> DbResult<Self> {
        if request.page_size == 0 {
            return Err(DatabaseError::invalid_request("Page size must be positive"));
        }

        let details = Arc::clone(&request.details);
        validate_sort(&request.sort, &details)?;

        let filter_predicate = filters_to_predicate(&request.filters, &details)?;
        let (order, mode) = ordering(&request.sort, &details);
        let fingerprint = fingerprint(&details, &request.filters, &request.sort);

        let position = match &request.continuation {
            None => None,
            Some(token) if token.fingerprint == fingerprint => Some(&token.position),
            Some(_) => {
                return Err(DatabaseError::new(
                    ErrorKind::InvalidContinuation,
                    "The page position no longer matches the table, filters, or sort",
                ));
            }
        };

        let (continuation_predicate, offset) = match (mode, position) {
            (_, None) => (Predicate::Always, 0),
            (PagingMode::Keyset, Some(Position::Keyset(values))) => {
                (keyset_after(&order, values)?, 0)
            }
            (PagingMode::Offset, Some(Position::Offset(offset))) => (Predicate::Always, *offset),
            _ => {
                return Err(DatabaseError::new(
                    ErrorKind::InvalidContinuation,
                    "The page position uses a different paging mode",
                ));
            }
        };

        Ok(Self {
            details,
            fingerprint,
            mode,
            order,
            predicate: Predicate::and(vec![filter_predicate, continuation_predicate]),
            page_size: request.page_size,
            max_bytes: request.max_bytes,
            offset,
        })
    }

    pub fn mode(&self) -> PagingMode {
        self.mode
    }

    /// Renders `SELECT data..., keys... FROM object WHERE ... ORDER BY ... LIMIT`.
    /// One extra row is requested to detect whether another page exists.
    pub fn render(&self, dialect: &dyn SqlDialect) -> DbResult<PageQuery> {
        let mut renderer = SqlRenderer::new(dialect, &self.details);

        let mut select_list: Vec<String> = self
            .details
            .columns
            .iter()
            .map(|column| renderer.select_expression(column))
            .collect::<DbResult<_>>()?;
        let data_columns = select_list.len();

        if self.mode == PagingMode::Keyset {
            for (position, term) in self.order.iter().enumerate() {
                select_list.push(renderer.key_expression(&term.column, position)?);
            }
        }

        let where_clause = renderer.render_predicate(&self.predicate)?;
        let order_clause = self
            .order
            .iter()
            .map(|term| renderer.render_order_term(term))
            .collect::<DbResult<Vec<_>>>()?;

        let mut text = format!(
            "SELECT {} FROM {} AS {} WHERE {where_clause}",
            select_list.join(", "),
            dialect.qualified_name(&self.details.object),
            dialect.quote_identifier(OBJECT_ALIAS),
        );

        if !order_clause.is_empty() {
            text.push_str(" ORDER BY ");
            text.push_str(&order_clause.join(", "));
        }

        text.push_str(&format!(" LIMIT {}", self.page_size + 1));

        if self.offset > 0 {
            text.push_str(&format!(" OFFSET {}", self.offset));
        }

        Ok(PageQuery {
            sql: RenderedSql {
                text,
                params: renderer.into_params(),
            },
            data_columns,
        })
    }

    /// Builds the page from fetched rows, enforcing page size and byte budget
    /// and computing the next continuation token.
    pub fn build_page(&self, columns: Vec<ColumnDescriptor>, raw_rows: Vec<Row>) -> Page {
        let data_columns = self.details.columns.len();
        let has_more_rows = raw_rows.len() > self.page_size;
        let mut rows = Vec::with_capacity(raw_rows.len().min(self.page_size));
        let mut last_key: Option<Vec<Value>> = None;
        let mut bytes = 0;
        let mut notice = None;

        for mut raw_row in raw_rows.into_iter().take(self.page_size) {
            let key = raw_row.split_off(data_columns.min(raw_row.len()));
            let row_bytes = estimated_row_bytes(&raw_row);

            if !rows.is_empty() && bytes + row_bytes > self.max_bytes {
                notice = Some(PageNotice::ByteLimit);
                break;
            }

            bytes += row_bytes;
            rows.push(raw_row);
            last_key = Some(key);
        }

        let has_more = has_more_rows || notice == Some(PageNotice::ByteLimit);
        let next_offset = self.offset + rows.len() as u64;

        let next = match self.mode {
            _ if !has_more => None,
            PagingMode::Keyset => last_key.map(|key| self.token(Position::Keyset(key))),
            PagingMode::Offset if next_offset >= MAX_OFFSET_ROWS => {
                notice = Some(PageNotice::OffsetLimitReached);
                None
            }
            PagingMode::Offset => Some(self.token(Position::Offset(next_offset))),
        };

        Page {
            columns: columns.into_iter().take(data_columns).collect(),
            rows,
            next,
            mode: self.mode,
            notice,
        }
    }

    fn token(&self, position: Position) -> ContinuationToken {
        ContinuationToken {
            fingerprint: self.fingerprint,
            position,
        }
    }
}

fn validate_sort(sort: &[SortKey], details: &ObjectDetails) -> DbResult<()> {
    for key in sort {
        if details.column(&key.column).is_none() {
            return Err(DatabaseError::invalid_request(format!(
                "Unknown sort column \"{}\"",
                key.column
            )));
        }
    }

    Ok(())
}

/// The full ordering: user keys, then a unique tie-breaker when available.
fn ordering(sort: &[SortKey], details: &ObjectDetails) -> (Vec<OrderTerm>, PagingMode) {
    let mut order: Vec<OrderTerm> = sort
        .iter()
        .map(|key| OrderTerm {
            column: ColumnRef::Column(key.column.clone()),
            direction: key.direction,
            nulls: key.nulls,
            is_non_null: details
                .column(&key.column)
                .is_some_and(|column| !column.nullable),
        })
        .collect();

    let tie_breaker: Option<Vec<ColumnRef>> = match details.unique_row_key() {
        Some(key) => Some(
            key.columns
                .iter()
                .map(|column| ColumnRef::Column(column.clone()))
                .collect(),
        ),
        None => details
            .row_locator
            .as_ref()
            .map(|_| vec![ColumnRef::Locator]),
    };

    let Some(tie_breaker) = tie_breaker else {
        return (order, PagingMode::Offset);
    };

    for column in tie_breaker {
        if !order.iter().any(|term| term.column == column) {
            order.push(OrderTerm {
                column,
                direction: SortDirection::Ascending,
                nulls: NullsOrder::Last,
                is_non_null: true,
            });
        }
    }

    (order, PagingMode::Keyset)
}

/// Rows strictly after `values` in the given order, with explicit NULL
/// placement: `OR` over each key of (earlier keys equal AND this key after).
fn keyset_after(order: &[OrderTerm], values: &[Value]) -> DbResult<Predicate> {
    if values.len() != order.len() {
        return Err(DatabaseError::new(
            ErrorKind::InvalidContinuation,
            "The page position does not match the ordering",
        ));
    }

    let mut alternatives = Vec::with_capacity(order.len());

    for (index, (term, value)) in order.iter().zip(values).enumerate() {
        let mut terms: Vec<Predicate> = order[..index]
            .iter()
            .zip(values)
            .map(|(earlier, earlier_value)| equal_to(&earlier.column, earlier_value))
            .collect();
        terms.push(after(term, value));
        alternatives.push(Predicate::and(terms));
    }

    Ok(Predicate::or(alternatives))
}

fn equal_to(column: &ColumnRef, value: &Value) -> Predicate {
    if value.is_null() {
        return Predicate::IsNull(column.clone());
    }

    Predicate::Compare {
        column: column.clone(),
        op: CompareOp::Equal,
        value: value.clone(),
    }
}

fn after(term: &OrderTerm, value: &Value) -> Predicate {
    let column = term.column.clone();

    if value.is_null() {
        return match term.nulls {
            NullsOrder::Last => Predicate::Never,
            NullsOrder::First => Predicate::IsNotNull(column),
        };
    }

    let op = match term.direction {
        SortDirection::Ascending => CompareOp::Greater,
        SortDirection::Descending => CompareOp::Less,
    };
    let beyond = Predicate::Compare {
        column: column.clone(),
        op,
        value: value.clone(),
    };

    match term.nulls {
        NullsOrder::Last if !term.is_non_null => {
            Predicate::or(vec![beyond, Predicate::IsNull(column)])
        }
        _ => beyond,
    }
}

fn fingerprint(details: &ObjectDetails, filters: &[Filter], sort: &[SortKey]) -> u64 {
    let mut hasher = DefaultHasher::new();
    details.object.hash(&mut hasher);
    details.revision().hash(&mut hasher);
    sort.hash(&mut hasher);

    for filter in filters {
        filter.column.hash(&mut hasher);
        filter.operator.hash(&mut hasher);

        match &filter.operand {
            FilterOperand::None => 0_u8.hash(&mut hasher),
            FilterOperand::Single(value) => hash_value(value, &mut hasher),
            FilterOperand::List(values) => values
                .iter()
                .for_each(|value| hash_value(value, &mut hasher)),
        }
    }

    hasher.finish()
}

fn hash_value(value: &Value, hasher: &mut DefaultHasher) {
    value.storage_name().hash(hasher);

    match value {
        Value::Float(number) => number.to_bits().hash(hasher),
        Value::Bytes(bytes) => bytes.hash(hasher),
        other => other.canonical_text().hash(hasher),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ColumnInfo, KeyInfo, RowLocator};
    use crate::filter::FilterOperator;
    use crate::ids::ConnectionId;
    use crate::resource::{ObjectKind, ObjectRef};
    use crate::sql::quote_identifier_ansi;

    struct TestDialect;

    impl SqlDialect for TestDialect {
        fn quote_identifier(&self, identifier: &str) -> String {
            quote_identifier_ansi(identifier)
        }

        fn qualified_name(&self, object: &ObjectRef) -> String {
            quote_identifier_ansi(&object.name)
        }

        fn placeholder(&self, index: usize, _column: Option<&ColumnInfo>) -> String {
            format!("?{index}")
        }

        fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String {
            format!("{expression} LIKE {pattern_placeholder} ESCAPE '\\'")
        }

        fn key_expression(&self, expression: &str, _column: Option<&ColumnInfo>) -> String {
            expression.to_owned()
        }
    }

    fn column(name: &str, ordinal: usize) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            type_name: "integer".to_owned(),
            cast_type: None,
            nullable: true,
            default: None,
            ordinal,
            decodable: true,
            comment: None,
        }
    }

    fn details(primary_key: Option<&[&str]>, locator: bool) -> Arc<ObjectDetails> {
        Arc::new(ObjectDetails {
            object: ObjectRef {
                connection: ConnectionId::from_uuid(uuid::Uuid::nil()),
                database: None,
                namespace: None,
                kind: ObjectKind::Table,
                name: "t".to_owned(),
            },
            columns: vec![column("a", 1), column("b", 2), column("c", 3)],
            primary_key: primary_key.map(|columns| KeyInfo {
                name: None,
                columns: columns.iter().map(|name| (*name).to_owned()).collect(),
                allows_null: false,
            }),
            unique_keys: Vec::new(),
            row_locator: locator.then(|| RowLocator {
                expression: "rowid".to_owned(),
                type_name: "integer".to_owned(),
            }),
            foreign_keys: Vec::new(),
            referencing_keys: Vec::new(),
            comment: None,
        })
    }

    fn row(values: &[i64]) -> Row {
        values.iter().map(|value| Value::Integer(*value)).collect()
    }

    fn request(details: Arc<ObjectDetails>, page_size: usize) -> PageRequest {
        PageRequest {
            page_size,
            ..PageRequest::first(details)
        }
    }

    #[test]
    fn primary_key_is_appended_as_tie_breaker() {
        let mut page_request = request(details(Some(&["a", "b"]), false), 10);
        page_request.sort = vec![SortKey::descending("c")];

        let plan = PagePlan::new(&page_request).expect("plan");
        let query = plan.render(&TestDialect).expect("render");

        assert_eq!(plan.mode(), PagingMode::Keyset);
        assert_eq!(
            query.sql.text,
            "SELECT o.\"a\", o.\"b\", o.\"c\", o.\"c\" AS \"qwerio_key_0\", o.\"a\" AS \"qwerio_key_1\", \
             o.\"b\" AS \"qwerio_key_2\" FROM \"t\" AS o WHERE 1 = 1 \
             ORDER BY o.\"c\" DESC NULLS LAST, o.\"a\" ASC NULLS LAST, o.\"b\" ASC NULLS LAST LIMIT 11"
                .replace("o.", "\"qwerio_object\".")
                .replace("AS o ", "AS \"qwerio_object\" ")
        );
        assert_eq!(query.data_columns, 3);
    }

    #[test]
    fn views_without_keys_use_offset_paging() {
        let plan = PagePlan::new(&request(details(None, false), 2)).expect("plan");
        let page = plan.build_page(
            Vec::new(),
            vec![row(&[1, 1, 1]), row(&[2, 2, 2]), row(&[3, 3, 3])],
        );

        assert_eq!(page.mode, PagingMode::Offset);
        assert_eq!(page.rows.len(), 2);

        let mut next_request = request(details(None, false), 2);
        next_request.continuation = page.next;
        let next_plan = PagePlan::new(&next_request).expect("next plan");
        let query = next_plan.render(&TestDialect).expect("render");

        assert!(query.sql.text.ends_with("LIMIT 3 OFFSET 2"));
    }

    #[test]
    fn row_locator_is_used_when_no_key_exists() {
        let plan = PagePlan::new(&request(details(None, true), 5)).expect("plan");
        let query = plan.render(&TestDialect).expect("render");

        assert_eq!(plan.mode(), PagingMode::Keyset);
        assert!(
            query
                .sql
                .text
                .contains("ORDER BY \"qwerio_object\".rowid ASC NULLS LAST")
        );
    }

    #[test]
    fn keyset_continuation_handles_nulls_and_composite_keys() {
        let mut first_request = request(details(Some(&["a"]), false), 1);
        first_request.sort = vec![SortKey::ascending("b")];
        let plan = PagePlan::new(&first_request).expect("plan");

        let page = plan.build_page(
            Vec::new(),
            vec![
                vec![
                    Value::Integer(7),
                    Value::Null,
                    Value::Integer(0),
                    Value::Null,
                    Value::Integer(7),
                ],
                row(&[8, 8, 8, 8, 8]),
            ],
        );
        assert_eq!(
            page.rows,
            vec![vec![Value::Integer(7), Value::Null, Value::Integer(0)]]
        );

        let mut next_request = first_request.clone();
        next_request.continuation = page.next;
        let next_query = PagePlan::new(&next_request)
            .expect("next plan")
            .render(&TestDialect)
            .expect("render");

        // After a NULL in a NULLS LAST key, only rows with the same NULL and a
        // greater tie-breaker follow.
        assert!(next_query.sql.text.contains(
            "WHERE (\"qwerio_object\".\"b\" IS NULL) AND (\"qwerio_object\".\"a\" > ?1)"
        ));
        assert_eq!(next_query.sql.params, vec![Value::Integer(7)]);
    }

    #[test]
    fn keyset_after_non_null_value_includes_trailing_nulls() {
        let order = vec![OrderTerm {
            column: ColumnRef::Column("b".to_owned()),
            direction: SortDirection::Ascending,
            nulls: NullsOrder::Last,
            is_non_null: false,
        }];

        let predicate = keyset_after(&order, &[Value::Integer(3)]).expect("predicate");

        assert_eq!(
            predicate,
            Predicate::Or(vec![
                Predicate::Compare {
                    column: ColumnRef::Column("b".to_owned()),
                    op: CompareOp::Greater,
                    value: Value::Integer(3),
                },
                Predicate::IsNull(ColumnRef::Column("b".to_owned())),
            ])
        );
    }

    #[test]
    fn last_page_has_no_continuation() {
        let plan = PagePlan::new(&request(details(Some(&["a"]), false), 5)).expect("plan");
        let page = plan.build_page(Vec::new(), vec![row(&[1, 1, 1, 1])]);

        assert!(page.next.is_none());
        assert_eq!(page.rows.len(), 1);
    }

    #[test]
    fn tokens_are_rejected_after_filters_change() {
        let base = request(details(Some(&["a"]), false), 1);
        let plan = PagePlan::new(&base).expect("plan");
        let page = plan.build_page(Vec::new(), vec![row(&[1, 1, 1, 1]), row(&[2, 2, 2, 2])]);

        let mut changed = base.clone();
        changed.filters = vec![Filter::new(
            "b",
            FilterOperator::IsNotNull,
            FilterOperand::None,
        )];
        changed.continuation = page.next;

        let error = PagePlan::new(&changed).expect_err("stale token");
        assert_eq!(error.kind, ErrorKind::InvalidContinuation);
    }

    #[test]
    fn tokens_are_rejected_after_metadata_changes() {
        let base = request(details(Some(&["a"]), false), 1);
        let page = PagePlan::new(&base)
            .expect("plan")
            .build_page(Vec::new(), vec![row(&[1, 1, 1, 1]), row(&[2, 2, 2, 2])]);

        let mut changed_details = (*details(Some(&["a"]), false)).clone();
        changed_details.columns.push(column("d", 4));
        let mut changed = request(Arc::new(changed_details), 1);
        changed.continuation = page.next;

        assert!(PagePlan::new(&changed).is_err());
    }

    #[test]
    fn byte_budget_cuts_page_and_continues_after_last_kept_row() {
        let mut page_request = request(details(Some(&["a"]), false), 10);
        page_request.max_bytes = estimated_row_bytes(&row(&[1, 1, 1]));
        let plan = PagePlan::new(&page_request).expect("plan");

        let page = plan.build_page(Vec::new(), vec![row(&[1, 1, 1, 1]), row(&[2, 2, 2, 2])]);

        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.notice, Some(PageNotice::ByteLimit));
        assert!(page.next.is_some());
    }

    #[test]
    fn offset_paging_stops_at_its_bound() {
        let page_size = 2;
        let plan = PagePlan {
            offset: MAX_OFFSET_ROWS - 2,
            ..PagePlan::new(&request(details(None, false), page_size)).expect("plan")
        };

        let page = plan.build_page(
            Vec::new(),
            vec![row(&[1, 1, 1]), row(&[2, 2, 2]), row(&[3, 3, 3])],
        );

        assert!(page.next.is_none());
        assert_eq!(page.notice, Some(PageNotice::OffsetLimitReached));
    }
}
