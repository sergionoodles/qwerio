//! Typed views over the catalog rows of a [`PermissionSnapshot`], for display.
//! Grantee, role, and member names match [`Principal::label`].

use crate::Value;
use crate::result::Row;

use super::PermissionSnapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectGrant {
    pub grantee: String,
    pub privilege: String,
    pub grantable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleMembership {
    pub role: String,
    pub member: String,
}

impl PermissionSnapshot {
    pub fn direct_grants(&self) -> Vec<DirectGrant> {
        self.grants
            .iter()
            .filter_map(|row| {
                Some(DirectGrant {
                    grantee: grantee_label(&cell(row, 0)?),
                    privilege: cell(row, 1)?,
                    grantable: cell(row, 2).is_some_and(|value| value == "YES"),
                })
            })
            .collect()
    }

    pub fn role_memberships(&self) -> Vec<RoleMembership> {
        self.memberships
            .iter()
            .filter_map(|row| {
                Some(RoleMembership {
                    role: cell(row, 0)?,
                    member: cell(row, 1)?,
                })
            })
            .collect()
    }
}

fn cell(row: &Row, index: usize) -> Option<String> {
    row.get(index).and_then(Value::canonical_text)
}

/// MySQL's information_schema quotes grantees as `'user'@'host'`, doubling
/// quotes inside either part.
fn grantee_label(raw: &str) -> String {
    let Some((user, host)) = raw
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .and_then(|inner| inner.split_once("'@'"))
    else {
        return raw.to_owned();
    };

    format!("{}@{}", user.replace("''", "'"), host.replace("''", "'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(grants: Vec<Row>, memberships: Vec<Row>) -> PermissionSnapshot {
        PermissionSnapshot {
            database: None,
            supports_roles: true,
            literal_database_names: false,
            principals: Vec::new(),
            memberships,
            grants,
            warnings: Vec::new(),
        }
    }

    fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    #[test]
    fn grantees_match_principal_labels_on_every_engine() {
        let grants = vec![
            vec![
                text("'ali''ce'@'%'"),
                text("SELECT"),
                text("YES"),
                text("Direct"),
            ],
            vec![
                text("bob@localhost"),
                text("INSERT"),
                text("NO"),
                text("Direct"),
            ],
            vec![text("PUBLIC"), text("USAGE"), text("NO"), text("postgres")],
        ];

        let grants = snapshot(grants, Vec::new()).direct_grants();

        assert_eq!(
            grants,
            [
                DirectGrant {
                    grantee: "ali'ce@%".to_owned(),
                    privilege: "SELECT".to_owned(),
                    grantable: true,
                },
                DirectGrant {
                    grantee: "bob@localhost".to_owned(),
                    privilege: "INSERT".to_owned(),
                    grantable: false,
                },
                DirectGrant {
                    grantee: "PUBLIC".to_owned(),
                    privilege: "USAGE".to_owned(),
                    grantable: false,
                },
            ]
        );
    }

    #[test]
    fn incomplete_rows_are_skipped() {
        let grants = vec![vec![text("alice")], vec![Value::Null, text("SELECT")]];
        let memberships = vec![vec![text("readers")]];

        let snapshot = snapshot(grants, memberships);

        assert!(snapshot.direct_grants().is_empty());
        assert!(snapshot.role_memberships().is_empty());
    }

    #[test]
    fn memberships_keep_role_and_member() {
        let memberships = vec![vec![text("readers"), text("alice@localhost")]];

        assert_eq!(
            snapshot(Vec::new(), memberships).role_memberships(),
            [RoleMembership {
                role: "readers".to_owned(),
                member: "alice@localhost".to_owned(),
            }]
        );
    }
}
