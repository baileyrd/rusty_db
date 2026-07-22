//! Diffs a set of `#[derive(Mapped)]` types' expected table shape (from
//! `Mapped::COLUMN_SPECS`/`PRIMARY_KEY`) against a live database's
//! reflected schema (`TableSchema`), generating the DDL statements
//! (rendered SQL text, for review — never executed automatically) needed
//! to reconcile them: a brand-new `CreateTable` (with its primary key, if
//! any) for an expected table missing from the database, and
//! `AlterTable::add_column`/`drop_column` for a column present on only
//! one side of an existing table.
//!
//! Deliberately conservative, in the same "starting point, not a
//! replacement" spirit as `automap`:
//! - **Never proposes dropping a whole table.** A table this diff
//!   doesn't recognize might not be tracked by the caller's `expected`
//!   list at all (a different part of the same app, this crate's own
//!   migration/audit-log bookkeeping tables, a schema shared with
//!   something else) — there's no way to tell "not currently mapped"
//!   from "meant to be deleted" from this list alone, so removing a
//!   `#[derive(Mapped)]` type's own table needs a hand-written
//!   `DropTable` migration.
//! - **Only diffs column presence, never type.** A live column's
//!   `type_name` is dialect-native, verbatim text (see `schema.rs`) with
//!   no portable representation to compare `ColumnType` against without
//!   reimplementing `automap::rust_type_for`'s heuristic in reverse, per
//!   dialect — changing a field's type without renaming it produces no
//!   suggested statement at all; review type-level changes by hand.
//! - **Never detects a rename.** Renaming a field is reported as one
//!   unrelated `AlterTable::drop_column` plus one unrelated
//!   `AlterTable::add_column` — running both, in that order, loses the
//!   column's data rather than preserving it. This is the same
//!   rename-blindness Alembic's own autogenerate is well known for.

use std::collections::HashMap;

use crate::dialect::Dialect;
use crate::mapping::{ColumnSpec, Mapped};
use crate::query::{AlterTable, CreateTable, ToSql};
use crate::schema::TableSchema;

/// One table's expected shape, built from a `#[derive(Mapped)]` type's
/// own `TABLE_NAME`/`COLUMN_SPECS`/`PRIMARY_KEY` — the caller builds one
/// `TableSpec` per type it wants `diff`/`Engine::autogenerate_migration`
/// to track.
#[derive(Debug, Clone)]
pub struct TableSpec {
    pub name: String,
    pub columns: Vec<ColumnSpec>,
    pub primary_key: Option<&'static str>,
}

impl TableSpec {
    pub fn of<T: Mapped>() -> Self {
        TableSpec {
            name: T::TABLE_NAME.to_string(),
            columns: T::COLUMN_SPECS.to_vec(),
            primary_key: T::PRIMARY_KEY,
        }
    }
}

/// Generates the DDL (as rendered SQL text, in the order it should run)
/// needed to bring `existing` in line with `expected` — see the module
/// doc for exactly what is and isn't detected. `existing` is keyed by
/// table name; a table in `expected` with no entry in `existing` is
/// treated as not existing yet (a brand-new `CreateTable`).
pub fn diff(
    dialect: &dyn Dialect,
    expected: &[TableSpec],
    existing: &HashMap<String, TableSchema>,
) -> Vec<String> {
    let mut statements = Vec::new();

    for table in expected {
        match existing.get(&table.name) {
            None => {
                let mut create = CreateTable::new(&table.name);
                for col in &table.columns {
                    create = create.column(col.name, col.ty);
                    if !col.nullable {
                        create = create.not_null();
                    }
                    if Some(col.name) == table.primary_key {
                        create = create.primary_key();
                    }
                }
                statements.push(create.to_sql(dialect).0);
            }
            Some(live) => {
                let live_columns: std::collections::HashSet<&str> =
                    live.columns.iter().map(|c| c.name.as_str()).collect();
                for col in &table.columns {
                    if !live_columns.contains(col.name) {
                        let mut alter = AlterTable::add_column(&table.name, col.name, col.ty);
                        if !col.nullable {
                            alter = alter.not_null();
                        }
                        statements.push(alter.to_sql(dialect).0);
                    }
                }

                let expected_columns: std::collections::HashSet<&str> =
                    table.columns.iter().map(|c| c.name).collect();
                for live_col in &live.columns {
                    if !expected_columns.contains(live_col.name.as_str()) {
                        statements.push(
                            AlterTable::drop_column(&table.name, &live_col.name)
                                .to_sql(dialect)
                                .0,
                        );
                    }
                }
            }
        }
    }

    statements
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::QuestionMarkDialect;
    use crate::query::ColumnType;
    use crate::schema::ColumnInfo;

    fn live_column(name: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            type_name: "whatever".to_string(),
            nullable,
            primary_key: false,
            default: None,
        }
    }

    fn spec(name: &'static str, ty: ColumnType, nullable: bool) -> ColumnSpec {
        ColumnSpec { name, ty, nullable }
    }

    fn table_spec(
        name: &str,
        columns: Vec<ColumnSpec>,
        primary_key: Option<&'static str>,
    ) -> TableSpec {
        TableSpec {
            name: name.to_string(),
            columns,
            primary_key,
        }
    }

    #[test]
    fn a_table_missing_from_the_database_gets_a_create_table() {
        let expected = vec![table_spec(
            "users",
            vec![
                spec("id", ColumnType::I64, false),
                spec("name", ColumnType::Text, false),
            ],
            Some("id"),
        )];
        let existing = HashMap::new();

        let statements = diff(&QuestionMarkDialect, &expected, &existing);
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0],
            r#"CREATE TABLE "users" ("id" INTEGER NOT NULL, "name" TEXT NOT NULL, PRIMARY KEY ("id"))"#
        );
    }

    #[test]
    fn an_up_to_date_table_generates_nothing() {
        let expected = vec![table_spec(
            "users",
            vec![spec("id", ColumnType::I64, false)],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        assert_eq!(
            diff(&QuestionMarkDialect, &expected, &existing),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_field_added_to_the_struct_becomes_an_add_column() {
        let expected = vec![table_spec(
            "users",
            vec![
                spec("id", ColumnType::I64, false),
                spec("nickname", ColumnType::Text, true),
            ],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        let statements = diff(&QuestionMarkDialect, &expected, &existing);
        assert_eq!(
            statements,
            vec![r#"ALTER TABLE "users" ADD COLUMN "nickname" TEXT"#.to_string()]
        );
    }

    #[test]
    fn a_field_removed_from_the_struct_becomes_a_drop_column() {
        let expected = vec![table_spec(
            "users",
            vec![spec("id", ColumnType::I64, false)],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false), live_column("legacy_flag", true)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        let statements = diff(&QuestionMarkDialect, &expected, &existing);
        assert_eq!(
            statements,
            vec![r#"ALTER TABLE "users" DROP COLUMN "legacy_flag""#.to_string()]
        );
    }

    #[test]
    fn a_live_table_absent_from_expected_is_never_proposed_for_dropping() {
        let expected = vec![table_spec(
            "users",
            vec![spec("id", ColumnType::I64, false)],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        // A tracked-elsewhere or bookkeeping table this diff never heard about.
        existing.insert(
            "_rusty_db_migrations".to_string(),
            TableSchema {
                name: "_rusty_db_migrations".to_string(),
                columns: vec![live_column("version", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        assert_eq!(
            diff(&QuestionMarkDialect, &expected, &existing),
            Vec::<String>::new()
        );
    }
}
