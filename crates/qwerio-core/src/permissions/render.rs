use super::*;

pub(super) fn identifier(engine: EngineKind, value: &str) -> DbResult<String> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(DatabaseError::invalid_request(
            "Enter a nonempty name without NUL characters",
        ));
    }
    Ok(engine.quote_identifier(value))
}

/// MySQL administration sessions enable NO_BACKSLASH_ESCAPES before using
/// these literals. PostgreSQL escape strings are independent of session mode.
pub(super) fn literal(engine: EngineKind, value: &str) -> DbResult<String> {
    if value.contains('\0') {
        return Err(DatabaseError::invalid_request(
            "NUL characters are not supported",
        ));
    }
    let quoted = value.replace('\'', "''");
    Ok(if engine == EngineKind::Postgres {
        format!("E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    })
}

pub(super) fn principal_sql(engine: EngineKind, principal: &Principal) -> DbResult<String> {
    let name = identifier(engine, &principal.name)?;
    match (engine, &principal.host) {
        (EngineKind::Postgres, None) | (EngineKind::Mariadb, None) => Ok(name),
        (EngineKind::Mysql | EngineKind::Mariadb, Some(host)) => {
            Ok(format!("{name}@{}", identifier(engine, host)?))
        }
        _ => Err(DatabaseError::invalid_request(
            "This account requires an explicit host",
        )),
    }
}

impl PermissionChange {
    /// Actual statements must only be passed to the dedicated administration
    /// session. Use `preview` for display; this method can contain a password.
    pub fn render(&self, engine: EngineKind) -> DbResult<String> {
        self.render_for_server(engine, false)
    }

    pub fn preview(&self, engine: EngineKind) -> DbResult<String> {
        self.preview_for_server(engine, false)
    }

    pub fn render_for_server(
        &self,
        engine: EngineKind,
        literal_database_names: bool,
    ) -> DbResult<String> {
        self.render_statement(engine, false, literal_database_names)
    }

    pub fn preview_for_server(
        &self,
        engine: EngineKind,
        literal_database_names: bool,
    ) -> DbResult<String> {
        self.render_statement(engine, true, literal_database_names)
    }

    fn render_statement(
        &self,
        engine: EngineKind,
        redact: bool,
        literal_database_names: bool,
    ) -> DbResult<String> {
        ensure_supported(engine)?;
        match self {
            Self::CreateUser {
                principal,
                password,
            } => {
                if engine == EngineKind::Mariadb && principal.host.is_none() {
                    return Err(DatabaseError::invalid_request(
                        "Enter an explicit host for the new user",
                    ));
                }
                if password.expose().is_empty() || password.expose().contains('\0') {
                    return Err(DatabaseError::invalid_request(
                        "Enter a password for the new user",
                    ));
                }
                let account = principal_sql(engine, principal)?;
                let password = literal(
                    engine,
                    if redact {
                        "[redacted]"
                    } else {
                        password.expose()
                    },
                )?;
                let clause = if engine == EngineKind::Postgres {
                    "WITH LOGIN PASSWORD"
                } else {
                    "IDENTIFIED BY"
                };
                Ok(format!("CREATE USER {account} {clause} {password}"))
            }
            Self::CreateRole { role } => {
                if engine == EngineKind::Mariadb && role.host.is_some() {
                    return Err(DatabaseError::invalid_request(
                        "MariaDB roles do not have hosts",
                    ));
                }
                Ok(format!("CREATE ROLE {}", principal_sql(engine, role)?))
            }
            Self::Membership {
                role,
                member,
                revoke,
            } => {
                let role = principal_sql(engine, role)?;
                let member = principal_sql(engine, member)?;
                let (verb, preposition) = if *revoke {
                    ("REVOKE", "FROM")
                } else {
                    ("GRANT", "TO")
                };
                Ok(format!("{verb} {role} {preposition} {member}"))
            }
            Self::DefaultRole { role, member } => {
                if engine == EngineKind::Postgres {
                    return Err(DatabaseError::new(
                        ErrorKind::Unsupported,
                        "PostgreSQL uses role inheritance instead of default role activation",
                    ));
                }
                let role = principal_sql(engine, role)?;
                let member = principal_sql(engine, member)?;
                let preposition = if engine == EngineKind::Mariadb {
                    "FOR"
                } else {
                    "TO"
                };
                Ok(format!("SET DEFAULT ROLE {role} {preposition} {member}"))
            }
            Self::Privilege {
                principal,
                scope,
                privilege,
                revoke,
                grant_option,
            } => {
                if !scope.privileges(engine).contains(&privilege.as_str()) {
                    return Err(DatabaseError::invalid_request(
                        "Choose a privilege supported by this scope",
                    ));
                }
                let account = if engine == EngineKind::Postgres
                    && principal.name == "PUBLIC"
                    && principal.host.is_none()
                {
                    "PUBLIC".to_owned()
                } else {
                    principal_sql(engine, principal)?
                };
                let target = scope.render_for_server(engine, literal_database_names)?;
                let privilege = if scope.kind == ScopeKind::Column {
                    format!("{privilege} ({})", identifier(engine, &scope.column)?)
                } else {
                    privilege.clone()
                };
                let (verb, preposition) = if *revoke {
                    ("REVOKE", "FROM")
                } else {
                    ("GRANT", "TO")
                };
                let suffix = if !revoke && *grant_option {
                    " WITH GRANT OPTION"
                } else {
                    ""
                };
                Ok(format!(
                    "{verb} {privilege} ON {target} {preposition} {account}{suffix}"
                ))
            }
        }
    }
}

impl PermissionScope {
    pub(super) fn render(&self, engine: EngineKind) -> DbResult<String> {
        self.render_for_server(engine, false)
    }

    fn render_for_server(
        &self,
        engine: EngineKind,
        literal_database_names: bool,
    ) -> DbResult<String> {
        if !Self::available_kinds(engine).contains(&self.kind) {
            return Err(DatabaseError::invalid_request(
                "This scope is not supported by the engine",
            ));
        }
        if self.kind == ScopeKind::Server {
            return Ok("*.*".to_owned());
        }
        let database = identifier(engine, &self.database)?;
        if self.kind == ScopeKind::Database {
            let database = if engine != EngineKind::Postgres && !literal_database_names {
                identifier(engine, &database_grant_pattern(&self.database))?
            } else {
                database
            };
            return Ok(if engine == EngineKind::Postgres {
                format!("DATABASE {database}")
            } else {
                format!("{database}.*")
            });
        }
        let namespace = if engine == EngineKind::Postgres {
            identifier(engine, &self.namespace)?
        } else {
            database
        };
        if self.kind == ScopeKind::Schema {
            return Ok(format!("SCHEMA {namespace}"));
        }
        let object = identifier(engine, &self.object)?;
        let kind = match self.kind {
            ScopeKind::Sequence => "SEQUENCE ",
            ScopeKind::Function => "FUNCTION ",
            ScopeKind::Procedure => "PROCEDURE ",
            _ if engine == EngineKind::Postgres => "TABLE ",
            _ => "",
        };
        Ok(format!("{kind}{namespace}.{object}"))
    }
}
