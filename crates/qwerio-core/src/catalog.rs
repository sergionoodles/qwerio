//! Object metadata returned by catalogs: columns, keys, and relationships.

use std::hash::{DefaultHasher, Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::resource::ObjectRef;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// Display type, e.g. `character varying(40)` or a SQLite declared type.
    pub type_name: String,
    /// A type expression the adapter can cast parameters to, when it differs
    /// from the display type (PostgreSQL drops type modifiers here).
    pub cast_type: Option<String>,
    pub nullable: bool,
    pub default: Option<String>,
    pub ordinal: usize,
    pub comment: Option<String>,
    /// Whether the adapter decodes this column's values natively. When false,
    /// browse queries read the engine's text form and cells become
    /// `Value::Unsupported` carrying that text.
    pub decodable: bool,
    /// Computed by the database (generated columns and identity columns that
    /// are always generated); inserts and updates must not set it.
    pub generated: bool,
}

/// An ordered set of columns, such as a primary key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyInfo {
    pub name: Option<String>,
    pub columns: Vec<String>,
    /// Some engines (SQLite rowid tables) accept NULL in declared key columns;
    /// such keys cannot order rows deterministically.
    pub allows_null: bool,
}

/// A declared foreign key. `columns[i]` references `referenced_columns[i]`;
/// the order is the constraint's declared order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub name: Option<String>,
    pub table: ObjectRef,
    pub columns: Vec<String>,
    pub referenced_table: ObjectRef,
    pub referenced_columns: Vec<String>,
}

/// A hidden unique row locator usable as a paging tie-breaker, such as
/// SQLite's `rowid`. It is selected under an alias and never shown as data.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RowLocator {
    /// The expression that reads the locator (already safe to embed).
    pub expression: String,
    pub type_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectDetails {
    pub object: ObjectRef,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Option<KeyInfo>,
    /// Declared unique keys over plain columns.
    pub unique_keys: Vec<KeyInfo>,
    pub row_locator: Option<RowLocator>,
    /// Foreign keys declared on this object.
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Foreign keys on other objects that reference this one.
    pub referencing_keys: Vec<ForeignKeyInfo>,
    pub comment: Option<String>,
}

impl ObjectDetails {
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// The first NULL-free key that uniquely identifies rows, preferring the
    /// primary key.
    pub fn unique_row_key(&self) -> Option<&KeyInfo> {
        self.primary_key
            .iter()
            .chain(&self.unique_keys)
            .find(|key| !key.allows_null)
    }

    /// A hash of the metadata. Paging tokens embed it so that a schema change
    /// invalidates continuation.
    pub fn revision(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}
