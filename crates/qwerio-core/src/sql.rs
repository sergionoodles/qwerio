//! Engine-neutral predicate trees and their rendering through an adapter's
//! [`SqlDialect`]. Shared code decides what a predicate means; the dialect
//! decides quoting, placeholders, casts, and case-insensitive matching.

use crate::catalog::{ColumnInfo, ObjectDetails};
use crate::error::{DatabaseError, DbResult};
use crate::filter::{NullsOrder, SortDirection};
use crate::resource::ObjectRef;
use crate::value::Value;

/// A column of the browsed object, or its hidden row locator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColumnRef {
    Column(String),
    Locator,
}

/// The columns that uniquely identify a row: a NULL-free unique key (the
/// primary key first), or else the hidden row locator. `None` when rows have
/// no identity, so they can be neither keyset-paged nor edited.
pub fn row_identity(details: &ObjectDetails) -> Option<Vec<ColumnRef>> {
    if let Some(key) = details.unique_row_key() {
        return Some(key.columns.iter().cloned().map(ColumnRef::Column).collect());
    }

    details
        .row_locator
        .as_ref()
        .map(|_| vec![ColumnRef::Locator])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareOp {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl CompareOp {
    pub fn sql(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    Always,
    Never,
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Compare {
        column: ColumnRef,
        op: CompareOp,
        value: Value,
    },
    IsNull(ColumnRef),
    IsNotNull(ColumnRef),
    /// Case-insensitive LIKE on the column's text form. The pattern uses
    /// [`crate::filter::LIKE_ESCAPE`] as its escape character.
    TextMatch {
        column: ColumnRef,
        pattern: String,
    },
    In {
        column: ColumnRef,
        values: Vec<Value>,
    },
}

impl Predicate {
    /// Conjunction that drops `Always` terms and short-circuits on `Never`.
    pub fn and(predicates: Vec<Predicate>) -> Predicate {
        let mut terms = Vec::with_capacity(predicates.len());

        for predicate in predicates {
            match predicate {
                Predicate::Always => {}
                Predicate::Never => return Predicate::Never,
                Predicate::And(inner) => terms.extend(inner),
                other => terms.push(other),
            }
        }

        match terms.len() {
            0 => Predicate::Always,
            1 => terms.remove(0),
            _ => Predicate::And(terms),
        }
    }

    /// Disjunction that drops `Never` terms and short-circuits on `Always`.
    pub fn or(predicates: Vec<Predicate>) -> Predicate {
        let mut terms = Vec::with_capacity(predicates.len());

        for predicate in predicates {
            match predicate {
                Predicate::Never => {}
                Predicate::Always => return Predicate::Always,
                Predicate::Or(inner) => terms.extend(inner),
                other => terms.push(other),
            }
        }

        match terms.len() {
            0 => Predicate::Never,
            1 => terms.remove(0),
            _ => Predicate::Or(terms),
        }
    }
}

/// Engine-specific SQL syntax supplied by each adapter.
pub trait SqlDialect {
    /// Quotes an identifier so any name, including reserved words, quotes, and
    /// mixed case, is used verbatim.
    fn quote_identifier(&self, identifier: &str) -> String;

    /// The fully qualified, quoted object name.
    fn qualified_name(&self, object: &ObjectRef) -> String;

    /// The placeholder expression for parameter `index` (1-based). `column`
    /// is the column the value is compared with, when known, so dialects can
    /// convert the bound value to the column's type.
    fn placeholder(&self, index: usize, column: Option<&ColumnInfo>) -> String;

    /// A case-insensitive LIKE comparing `expression`'s text form with the
    /// pattern placeholder, using `\` as the escape character.
    fn case_insensitive_like(&self, expression: &str, pattern_placeholder: &str) -> String;

    /// An expression selected alongside a row to capture an ordering key for
    /// continuation. It must round-trip through [`Self::placeholder`].
    fn key_expression(&self, expression: &str, column: Option<&ColumnInfo>) -> String;

    /// The select-list entry for a data column read through `expression`.
    /// Dialects may read columns they cannot decode in a text form; the entry
    /// must keep the column's name.
    fn select_expression(&self, expression: &str, _column: &ColumnInfo) -> String {
        expression.to_owned()
    }
}

/// SQL text plus the values bound to its placeholders, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedSql {
    pub text: String,
    pub params: Vec<Value>,
}

/// Alias of the browsed object in generated queries. Column references are
/// qualified with it, so they never resolve to select-list aliases.
pub const OBJECT_ALIAS: &str = "qwerio_object";

/// Renders predicates and ordering for one object.
pub struct SqlRenderer<'a> {
    dialect: &'a dyn SqlDialect,
    details: &'a ObjectDetails,
    params: Vec<Value>,
}

impl<'a> SqlRenderer<'a> {
    pub fn new(dialect: &'a dyn SqlDialect, details: &'a ObjectDetails) -> Self {
        Self {
            dialect,
            details,
            params: Vec::new(),
        }
    }

    pub fn into_params(self) -> Vec<Value> {
        self.params
    }

    pub fn column_expression(&self, column: &ColumnRef) -> DbResult<String> {
        match column {
            ColumnRef::Column(name) => {
                if self.details.column(name).is_none() {
                    return Err(DatabaseError::invalid_request(format!(
                        "Unknown column \"{name}\""
                    )));
                }

                Ok(format!(
                    "{}.{}",
                    self.dialect.quote_identifier(OBJECT_ALIAS),
                    self.dialect.quote_identifier(name)
                ))
            }
            ColumnRef::Locator => self
                .details
                .row_locator
                .as_ref()
                .map(|locator| {
                    format!(
                        "{}.{}",
                        self.dialect.quote_identifier(OBJECT_ALIAS),
                        locator.expression
                    )
                })
                .ok_or_else(|| DatabaseError::internal("This object has no row locator")),
        }
    }

    fn column_info(&self, column: &ColumnRef) -> Option<&'a ColumnInfo> {
        match column {
            ColumnRef::Column(name) => self.details.column(name),
            ColumnRef::Locator => None,
        }
    }

    /// Binds a parameter and returns its placeholder, converted to `column`'s
    /// type when given.
    pub fn bind(&mut self, value: Value, column: Option<&ColumnInfo>) -> String {
        self.params.push(value);
        self.dialect.placeholder(self.params.len(), column)
    }

    pub fn render_predicate(&mut self, predicate: &Predicate) -> DbResult<String> {
        let sql = match predicate {
            Predicate::Always => "1 = 1".to_owned(),
            Predicate::Never => "1 = 0".to_owned(),
            Predicate::And(terms) => self.render_terms(terms, " AND ")?,
            Predicate::Or(terms) => self.render_terms(terms, " OR ")?,
            Predicate::Compare { column, op, value } => {
                let expression = self.column_expression(column)?;
                let placeholder = self.bind(value.clone(), self.column_info(column));
                format!("{expression} {} {placeholder}", op.sql())
            }
            Predicate::IsNull(column) => format!("{} IS NULL", self.column_expression(column)?),
            Predicate::IsNotNull(column) => {
                format!("{} IS NOT NULL", self.column_expression(column)?)
            }
            Predicate::TextMatch { column, pattern } => {
                let expression = self.column_expression(column)?;
                let placeholder = self.bind(Value::Text(pattern.clone()), None);
                self.dialect
                    .case_insensitive_like(&expression, &placeholder)
            }
            Predicate::In { column, values } => {
                let expression = self.column_expression(column)?;
                let info = self.column_info(column);
                let placeholders: Vec<String> = values
                    .iter()
                    .map(|value| self.bind(value.clone(), info))
                    .collect();
                format!("{expression} IN ({})", placeholders.join(", "))
            }
        };

        Ok(sql)
    }

    fn render_terms(&mut self, terms: &[Predicate], separator: &str) -> DbResult<String> {
        let rendered = terms
            .iter()
            .map(|term| self.render_predicate(term).map(|sql| format!("({sql})")))
            .collect::<DbResult<Vec<_>>>()?;

        Ok(rendered.join(separator))
    }

    pub fn render_order_term(&self, term: &OrderTerm) -> DbResult<String> {
        let expression = self.column_expression(&term.column)?;
        let direction = match term.direction {
            SortDirection::Ascending => "ASC",
            SortDirection::Descending => "DESC",
        };
        let nulls = match term.nulls {
            NullsOrder::First => "NULLS FIRST",
            NullsOrder::Last => "NULLS LAST",
        };

        Ok(format!("{expression} {direction} {nulls}"))
    }

    /// A key column for continuation, under a unique alias.
    pub fn key_expression(&self, column: &ColumnRef, position: usize) -> DbResult<String> {
        let expression = self.column_expression(column)?;
        let key = self
            .dialect
            .key_expression(&expression, self.column_info(column));
        let alias = self
            .dialect
            .quote_identifier(&format!("qwerio_key_{position}"));

        Ok(format!("{key} AS {alias}"))
    }

    /// A data column's select-list entry.
    pub fn select_expression(&self, column: &ColumnInfo) -> DbResult<String> {
        let expression = self.column_expression(&ColumnRef::Column(column.name.clone()))?;
        Ok(self.dialect.select_expression(&expression, column))
    }
}

/// One ORDER BY term over a column or the row locator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OrderTerm {
    pub column: ColumnRef,
    pub direction: SortDirection,
    pub nulls: NullsOrder,
    /// The column cannot hold NULL, so continuation needs no NULL branch.
    pub is_non_null: bool,
}

/// Quotes an identifier with double quotes, doubling embedded quotes. Both
/// PostgreSQL and SQLite accept this standard form.
pub fn quote_identifier_ansi(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn and_flattens_and_short_circuits() {
        let null_check = Predicate::IsNull(ColumnRef::Column("a".to_owned()));

        assert_eq!(Predicate::and(vec![]), Predicate::Always);
        assert_eq!(
            Predicate::and(vec![Predicate::Always, null_check.clone()]),
            null_check
        );
        assert_eq!(
            Predicate::and(vec![null_check.clone(), Predicate::Never]),
            Predicate::Never
        );
    }

    #[test]
    fn or_drops_never_terms() {
        let null_check = Predicate::IsNull(ColumnRef::Column("a".to_owned()));

        assert_eq!(Predicate::or(vec![]), Predicate::Never);
        assert_eq!(
            Predicate::or(vec![Predicate::Never, null_check.clone()]),
            null_check
        );
    }

    #[test]
    fn identifiers_are_quoted_verbatim() {
        assert_eq!(
            quote_identifier_ansi("Weird \"name\""),
            "\"Weird \"\"name\"\"\""
        );
        assert_eq!(quote_identifier_ansi("select"), "\"select\"");
    }
}
