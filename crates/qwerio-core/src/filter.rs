//! Structured record filters and sorting with engine-neutral meaning.
//!
//! Filters translate into [`Predicate`] trees here; adapters only render them.

use serde::{Deserialize, Serialize};

use crate::catalog::ObjectDetails;
use crate::error::{DatabaseError, DbResult};
use crate::sql::{ColumnRef, CompareOp, Predicate};
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterOperator {
    Equals,
    /// NULL-aware: rows where the column is NULL also match.
    NotEquals,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    /// Case-insensitive substring match on the value's text form. SQLite folds
    /// ASCII letters only; see `DatabaseCapabilities::unicode_case_folding`.
    Contains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
    /// Matches any of a non-empty list of non-NULL values.
    In,
}

impl FilterOperator {
    pub const ALL: [Self; 12] = [
        Self::Equals,
        Self::NotEquals,
        Self::Less,
        Self::LessOrEqual,
        Self::Greater,
        Self::GreaterOrEqual,
        Self::Contains,
        Self::StartsWith,
        Self::EndsWith,
        Self::IsNull,
        Self::IsNotNull,
        Self::In,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Equals => "=",
            Self::NotEquals => "≠",
            Self::Less => "<",
            Self::LessOrEqual => "≤",
            Self::Greater => ">",
            Self::GreaterOrEqual => "≥",
            Self::Contains => "contains",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::IsNull => "is NULL",
            Self::IsNotNull => "is not NULL",
            Self::In => "in",
        }
    }

    pub fn operand_arity(self) -> OperandArity {
        match self {
            Self::IsNull | Self::IsNotNull => OperandArity::None,
            Self::In => OperandArity::List,
            _ => OperandArity::Single,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperandArity {
    None,
    Single,
    List,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FilterOperand {
    None,
    /// A single value. User-entered text is `Value::Text`; adapters convert it
    /// to the column type (PostgreSQL casts, SQLite applies column affinity).
    Single(Value),
    List(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub column: String,
    pub operator: FilterOperator,
    pub operand: FilterOperand,
}

impl Filter {
    pub fn new(
        column: impl Into<String>,
        operator: FilterOperator,
        operand: FilterOperand,
    ) -> Self {
        Self {
            column: column.into(),
            operator,
            operand,
        }
    }

    pub fn equals(column: impl Into<String>, value: Value) -> Self {
        Self::new(column, FilterOperator::Equals, FilterOperand::Single(value))
    }

    pub fn is_null(column: impl Into<String>) -> Self {
        Self::new(column, FilterOperator::IsNull, FilterOperand::None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SortDirection {
    Ascending,
    Descending,
}

impl SortDirection {
    pub fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

/// Explicit NULL placement, so both engines order identically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NullsOrder {
    First,
    #[default]
    Last,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortKey {
    pub column: String,
    pub direction: SortDirection,
    pub nulls: NullsOrder,
}

impl SortKey {
    pub fn ascending(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Ascending,
            nulls: NullsOrder::Last,
        }
    }

    pub fn descending(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            direction: SortDirection::Descending,
            nulls: NullsOrder::Last,
        }
    }
}

/// Converts filters into one predicate (all filters must match).
pub fn filters_to_predicate(filters: &[Filter], details: &ObjectDetails) -> DbResult<Predicate> {
    let predicates = filters
        .iter()
        .map(|filter| filter_to_predicate(filter, details))
        .collect::<DbResult<Vec<_>>>()?;

    Ok(Predicate::and(predicates))
}

fn filter_to_predicate(filter: &Filter, details: &ObjectDetails) -> DbResult<Predicate> {
    if details.column(&filter.column).is_none() {
        return Err(DatabaseError::invalid_request(format!(
            "Unknown column \"{}\" in filter",
            filter.column
        )));
    }

    let column = ColumnRef::Column(filter.column.clone());

    match (filter.operator, &filter.operand) {
        (FilterOperator::IsNull, FilterOperand::None) => Ok(Predicate::IsNull(column)),
        (FilterOperator::IsNotNull, FilterOperand::None) => Ok(Predicate::IsNotNull(column)),
        (FilterOperator::In, FilterOperand::List(values)) => in_predicate(column, values),
        (operator, FilterOperand::Single(value)) => single_value_predicate(column, operator, value),
        (operator, _) => Err(DatabaseError::invalid_request(format!(
            "The \"{}\" filter has the wrong number of values",
            operator.label()
        ))),
    }
}

fn in_predicate(column: ColumnRef, values: &[Value]) -> DbResult<Predicate> {
    if values.is_empty() {
        return Err(DatabaseError::invalid_request(
            "The \"in\" filter needs at least one value",
        ));
    }

    if values.iter().any(Value::is_null) {
        return Err(DatabaseError::invalid_request(
            "Use \"is NULL\" to match NULL values",
        ));
    }

    Ok(Predicate::In {
        column,
        values: values.to_vec(),
    })
}

fn single_value_predicate(
    column: ColumnRef,
    operator: FilterOperator,
    value: &Value,
) -> DbResult<Predicate> {
    if value.is_null() {
        return Err(DatabaseError::invalid_request(
            "Use \"is NULL\" or \"is not NULL\" to compare with NULL",
        ));
    }

    let compare = |op| Predicate::Compare {
        column: column.clone(),
        op,
        value: value.clone(),
    };

    let predicate = match operator {
        FilterOperator::Equals => compare(CompareOp::Equal),
        FilterOperator::NotEquals => Predicate::or(vec![
            compare(CompareOp::NotEqual),
            Predicate::IsNull(column.clone()),
        ]),
        FilterOperator::Less => compare(CompareOp::Less),
        FilterOperator::LessOrEqual => compare(CompareOp::LessOrEqual),
        FilterOperator::Greater => compare(CompareOp::Greater),
        FilterOperator::GreaterOrEqual => compare(CompareOp::GreaterOrEqual),
        FilterOperator::Contains => text_match(column, value, MatchPosition::Anywhere)?,
        FilterOperator::StartsWith => text_match(column, value, MatchPosition::Start)?,
        FilterOperator::EndsWith => text_match(column, value, MatchPosition::End)?,
        FilterOperator::IsNull | FilterOperator::IsNotNull | FilterOperator::In => {
            return Err(DatabaseError::invalid_request(format!(
                "The \"{}\" filter has the wrong number of values",
                operator.label()
            )));
        }
    };

    Ok(predicate)
}

enum MatchPosition {
    Start,
    End,
    Anywhere,
}

fn text_match(column: ColumnRef, value: &Value, position: MatchPosition) -> DbResult<Predicate> {
    let Some(text) = value.canonical_text() else {
        return Err(DatabaseError::invalid_request(
            "Text filters need a text value",
        ));
    };

    let escaped = escape_like(&text);
    let pattern = match position {
        MatchPosition::Start => format!("{escaped}%"),
        MatchPosition::End => format!("%{escaped}"),
        MatchPosition::Anywhere => format!("%{escaped}%"),
    };

    Ok(Predicate::TextMatch { column, pattern })
}

/// The escape character used in LIKE patterns produced by filters.
pub const LIKE_ESCAPE: char = '\\';

fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        if matches!(character, '%' | '_' | LIKE_ESCAPE) {
            escaped.push(LIKE_ESCAPE);
        }

        escaped.push(character);
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ColumnInfo;
    use crate::ids::ConnectionId;
    use crate::resource::{ObjectKind, ObjectRef};

    pub(crate) fn sample_details() -> ObjectDetails {
        let column = |name: &str, ordinal| ColumnInfo {
            name: name.to_owned(),
            type_name: "text".to_owned(),
            cast_type: None,
            nullable: true,
            default: None,
            ordinal,
            decodable: true,
            generated: false,
            comment: None,
        };

        ObjectDetails {
            object: ObjectRef {
                connection: ConnectionId::new(),
                database: None,
                namespace: None,
                kind: ObjectKind::Table,
                name: "items".to_owned(),
            },
            columns: vec![column("id", 1), column("name", 2)],
            primary_key: None,
            unique_keys: Vec::new(),
            row_locator: None,
            foreign_keys: Vec::new(),
            referencing_keys: Vec::new(),
            comment: None,
        }
    }

    #[test]
    fn not_equals_includes_null_rows() {
        let filter = Filter::new(
            "name",
            FilterOperator::NotEquals,
            FilterOperand::Single(Value::Text("a".to_owned())),
        );
        let predicate = filters_to_predicate(&[filter], &sample_details());
        let column = ColumnRef::Column("name".to_owned());

        assert_eq!(
            predicate,
            Ok(Predicate::Or(vec![
                Predicate::Compare {
                    column: column.clone(),
                    op: CompareOp::NotEqual,
                    value: Value::Text("a".to_owned()),
                },
                Predicate::IsNull(column),
            ]))
        );
    }

    #[test]
    fn comparing_with_null_is_rejected() {
        let result =
            filters_to_predicate(&[Filter::equals("name", Value::Null)], &sample_details());

        assert!(result.is_err());
    }

    #[test]
    fn unknown_columns_are_rejected() {
        let result = filters_to_predicate(&[Filter::is_null("missing")], &sample_details());

        assert!(result.is_err());
    }

    #[test]
    fn contains_escapes_like_wildcards() {
        let filter = Filter::new(
            "name",
            FilterOperator::Contains,
            FilterOperand::Single(Value::Text("50%_off\\".to_owned())),
        );
        let predicate = filters_to_predicate(&[filter], &sample_details());

        assert_eq!(
            predicate,
            Ok(Predicate::TextMatch {
                column: ColumnRef::Column("name".to_owned()),
                pattern: "%50\\%\\_off\\\\%".to_owned(),
            })
        );
    }

    #[test]
    fn in_rejects_empty_and_null_lists() {
        let empty = Filter::new("id", FilterOperator::In, FilterOperand::List(Vec::new()));
        let with_null = Filter::new(
            "id",
            FilterOperator::In,
            FilterOperand::List(vec![Value::Null]),
        );

        assert!(filters_to_predicate(&[empty], &sample_details()).is_err());
        assert!(filters_to_predicate(&[with_null], &sample_details()).is_err());
    }

    #[test]
    fn operand_arity_mismatch_is_rejected() {
        let filter = Filter::new(
            "id",
            FilterOperator::IsNull,
            FilterOperand::Single(Value::Integer(1)),
        );

        assert!(filters_to_predicate(&[filter], &sample_details()).is_err());
    }

    #[test]
    fn no_filters_match_everything() {
        assert_eq!(
            filters_to_predicate(&[], &sample_details()),
            Ok(Predicate::Always)
        );
    }
}
