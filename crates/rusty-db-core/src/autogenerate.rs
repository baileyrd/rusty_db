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
//! replacement" spirit as `automap` — and, for the two things below that
//! genuinely can't be inferred from `expected`/`existing` alone, the fix
//! isn't a smarter heuristic but an explicit, opt-in `AutogenerateOptions`
//! the caller fills in by hand:
//! - **Never proposes dropping a whole table, unless explicitly
//!   allow-listed.** A table this diff doesn't recognize might not be
//!   tracked by the caller's `expected` list at all (a different part of
//!   the same app, this crate's own migration/audit-log bookkeeping
//!   tables, a schema shared with something else) — there's no way to
//!   tell "not currently mapped" from "meant to be deleted" from this
//!   list alone. `AutogenerateOptions::allow_drop_tables` is exactly that
//!   missing say-so: name a table there and, if it's live but absent from
//!   `expected`, a `DropTable` is proposed for it; leave it off (the
//!   default) and an unrecognized live table is left completely alone.
//! - **Never detects a rename, unless explicitly hinted.** Renaming a
//!   field is otherwise reported as one unrelated `AlterTable::drop_column`
//!   plus one unrelated `AlterTable::add_column` — running both, in that
//!   order, loses the column's data rather than preserving it, the same
//!   rename-blindness Alembic's own autogenerate is well known for.
//!   `AutogenerateOptions::renamed_columns` lets the caller say "this
//!   table's column was renamed from X to Y" up front, producing a single
//!   data-preserving `AlterTable::rename_column` instead — but only when
//!   the hint actually matches what's live (the old name still present,
//!   the new name expected but not yet live); a stale or irrelevant hint
//!   is simply ignored rather than forced.
//!
//! - **Never detects a type change, unless explicitly hinted, and even
//!   then only on Postgres.** A live column's `type_name` is
//!   dialect-native, verbatim text (see `schema.rs`) with no portable
//!   representation to compare `ColumnType` against without reimplementing
//!   `automap::rust_type_for`'s heuristic in reverse, per dialect — so
//!   instead of guessing, `AutogenerateOptions::changed_column_types` lets
//!   the caller say "this table's column genuinely changed type" up
//!   front, the same up-front-confirmation shape `renamed_columns` already
//!   has (only acted on when the column is actually live on both sides —
//!   a stale hint naming a column that no longer exists is simply
//!   ignored). Unlike renames, though, this only ever produces a
//!   statement on Postgres: `AlterTable::alter_column_type` panics on any
//!   dialect where `Dialect::supports_alter_column_type()` is `false`
//!   (MySQL/MariaDB's `MODIFY COLUMN` would silently reset any omitted
//!   nullability/default, and SQLite has no direct support at all — see
//!   that method's own doc), so `diff` checks the capability first and
//!   quietly skips emitting anything for a hinted column when it's
//!   `false`, rather than reaching that panic.

use std::collections::HashMap;

use crate::dialect::Dialect;
use crate::mapping::{ColumnSpec, Mapped};
use crate::query::{AlterTable, CreateTable, DropTable, ToSql};
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

/// Explicit, opt-in inputs to `diff`/`Engine::autogenerate_migration` for
/// the two things it can't safely infer from `expected`/`existing` alone
/// — see the module doc for why each needs a caller's say-so rather than
/// a heuristic. All three default to empty, giving back exactly v1's
/// conservative behavior.
#[derive(Debug, Clone, Default)]
pub struct AutogenerateOptions {
    /// `(table, old_column_name, new_column_name)` hints. Only acted on
    /// when the hint actually matches what's live: `old_column_name` is
    /// still a live column of `table` and `new_column_name` is expected
    /// but not already live — otherwise it's silently ignored rather than
    /// forced onto a shape it no longer describes.
    pub renamed_columns: Vec<(String, String, String)>,
    /// Table names allowed to be proposed for a whole `DropTable` if
    /// they're live but absent from `expected`. A live table *not* named
    /// here is always left alone, regardless of what `expected` contains.
    pub allow_drop_tables: Vec<String>,
    /// `(table, column)` hints confirming a column's type genuinely
    /// changed. Only acted on when the column is a live column of `table`
    /// *and* still expected (same name on both sides — a rename or
    /// add/drop is a different hint) — an otherwise-stale hint is simply
    /// ignored. Even a matching hint only ever produces a statement when
    /// `dialect.supports_alter_column_type()` is `true` (Postgres only,
    /// for now); see the module doc for why.
    pub changed_column_types: Vec<(String, String)>,
}

impl AutogenerateOptions {
    fn renamed_column_for<'a>(
        &'a self,
        table: &str,
        live_columns: &std::collections::HashSet<&str>,
        expected_columns: &std::collections::HashSet<&str>,
    ) -> HashMap<&'a str, &'a str> {
        self.renamed_columns
            .iter()
            .filter(|(t, old_name, new_name)| {
                t == table
                    && live_columns.contains(old_name.as_str())
                    && expected_columns.contains(new_name.as_str())
                    && !live_columns.contains(new_name.as_str())
            })
            .map(|(_, old_name, new_name)| (old_name.as_str(), new_name.as_str()))
            .collect()
    }

    fn changed_column_types_for<'a>(
        &'a self,
        table: &str,
        live_columns: &std::collections::HashSet<&str>,
        expected_columns: &std::collections::HashSet<&str>,
    ) -> std::collections::HashSet<&'a str> {
        self.changed_column_types
            .iter()
            .filter(|(t, column)| {
                t == table
                    && live_columns.contains(column.as_str())
                    && expected_columns.contains(column.as_str())
            })
            .map(|(_, column)| column.as_str())
            .collect()
    }
}

/// Generates the DDL (as rendered SQL text, in the order it should run)
/// needed to bring `existing` in line with `expected` — see the module
/// doc for exactly what is and isn't detected, and what `options` lets a
/// caller opt into beyond that. `existing` is keyed by table name; a
/// table in `expected` with no entry in `existing` is treated as not
/// existing yet (a brand-new `CreateTable`). `existing` may also contain
/// tables absent from `expected` — those are only ever considered for
/// `options.allow_drop_tables`, never anything else.
pub fn diff(
    dialect: &dyn Dialect,
    expected: &[TableSpec],
    existing: &HashMap<String, TableSchema>,
    options: &AutogenerateOptions,
) -> Vec<String> {
    let mut statements = Vec::new();
    let mut expected_names: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for table in expected {
        expected_names.insert(table.name.as_str());
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
                let expected_columns: std::collections::HashSet<&str> =
                    table.columns.iter().map(|c| c.name).collect();
                let renamed_from =
                    options.renamed_column_for(&table.name, &live_columns, &expected_columns);
                let renamed_to: HashMap<&str, &str> =
                    renamed_from.iter().map(|(&old, &new)| (new, old)).collect();

                for (&old_name, &new_name) in &renamed_from {
                    statements.push(
                        AlterTable::rename_column(&table.name, old_name, new_name)
                            .to_sql(dialect)
                            .0,
                    );
                }

                for col in &table.columns {
                    if !live_columns.contains(col.name) && !renamed_to.contains_key(col.name) {
                        let mut alter = AlterTable::add_column(&table.name, col.name, col.ty);
                        if !col.nullable {
                            alter = alter.not_null();
                        }
                        statements.push(alter.to_sql(dialect).0);
                    }
                }

                for live_col in &live.columns {
                    if !expected_columns.contains(live_col.name.as_str())
                        && !renamed_from.contains_key(live_col.name.as_str())
                    {
                        statements.push(
                            AlterTable::drop_column(&table.name, &live_col.name)
                                .to_sql(dialect)
                                .0,
                        );
                    }
                }

                if dialect.supports_alter_column_type() {
                    let changed_types = options.changed_column_types_for(
                        &table.name,
                        &live_columns,
                        &expected_columns,
                    );
                    for col in &table.columns {
                        if changed_types.contains(col.name) {
                            statements.push(
                                AlterTable::alter_column_type(&table.name, col.name, col.ty)
                                    .to_sql(dialect)
                                    .0,
                            );
                        }
                    }
                }
            }
        }
    }

    for table_name in existing.keys() {
        if !expected_names.contains(table_name.as_str())
            && options.allow_drop_tables.iter().any(|t| t == table_name)
        {
            statements.push(DropTable::new(table_name).to_sql(dialect).0);
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

        let statements = diff(
            &QuestionMarkDialect,
            &expected,
            &existing,
            &AutogenerateOptions::default(),
        );
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
            diff(
                &QuestionMarkDialect,
                &expected,
                &existing,
                &AutogenerateOptions::default(),
            ),
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

        let statements = diff(
            &QuestionMarkDialect,
            &expected,
            &existing,
            &AutogenerateOptions::default(),
        );
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

        let statements = diff(
            &QuestionMarkDialect,
            &expected,
            &existing,
            &AutogenerateOptions::default(),
        );
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
            diff(
                &QuestionMarkDialect,
                &expected,
                &existing,
                &AutogenerateOptions::default(),
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_matching_rename_hint_produces_a_rename_column_instead_of_drop_plus_add() {
        let expected = vec![table_spec(
            "users",
            vec![
                spec("id", ColumnType::I64, false),
                spec("display_name", ColumnType::Text, false),
            ],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false), live_column("nickname", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        let options = AutogenerateOptions {
            renamed_columns: vec![(
                "users".to_string(),
                "nickname".to_string(),
                "display_name".to_string(),
            )],
            allow_drop_tables: Vec::new(),
            changed_column_types: Vec::new(),
        };

        let statements = diff(&QuestionMarkDialect, &expected, &existing, &options);
        assert_eq!(
            statements,
            vec![r#"ALTER TABLE "users" RENAME COLUMN "nickname" TO "display_name""#.to_string()]
        );
    }

    #[test]
    fn a_stale_rename_hint_is_ignored_and_falls_back_to_drop_plus_add() {
        // The hint claims "foo was renamed to bar", but "foo" isn't actually
        // live anymore (maybe it really was already renamed, or the hint is
        // just wrong) — it shouldn't be forced onto a shape that no longer
        // matches; ordinary add/drop diffing takes over instead.
        let expected = vec![table_spec(
            "users",
            vec![
                spec("id", ColumnType::I64, false),
                spec("bar", ColumnType::Text, false),
            ],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false), live_column("baz", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        let options = AutogenerateOptions {
            renamed_columns: vec![("users".to_string(), "foo".to_string(), "bar".to_string())],
            allow_drop_tables: Vec::new(),
            changed_column_types: Vec::new(),
        };

        let mut statements = diff(&QuestionMarkDialect, &expected, &existing, &options);
        statements.sort();
        assert_eq!(
            statements,
            vec![
                r#"ALTER TABLE "users" ADD COLUMN "bar" TEXT NOT NULL"#.to_string(),
                r#"ALTER TABLE "users" DROP COLUMN "baz""#.to_string(),
            ]
        );
    }

    #[test]
    fn an_allow_listed_table_absent_from_expected_gets_a_drop_table() {
        let expected: Vec<TableSpec> = Vec::new();
        let mut existing = HashMap::new();
        existing.insert(
            "legacy_sessions".to_string(),
            TableSchema {
                name: "legacy_sessions".to_string(),
                columns: vec![live_column("id", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        let options = AutogenerateOptions {
            renamed_columns: Vec::new(),
            allow_drop_tables: vec!["legacy_sessions".to_string()],
            changed_column_types: Vec::new(),
        };

        let statements = diff(&QuestionMarkDialect, &expected, &existing, &options);
        assert_eq!(
            statements,
            vec![r#"DROP TABLE "legacy_sessions""#.to_string()]
        );
    }

    #[test]
    fn a_live_table_absent_from_expected_and_not_allow_listed_is_left_alone() {
        let expected: Vec<TableSpec> = Vec::new();
        let mut existing = HashMap::new();
        existing.insert(
            "legacy_sessions".to_string(),
            TableSchema {
                name: "legacy_sessions".to_string(),
                columns: vec![live_column("id", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        assert_eq!(
            diff(
                &QuestionMarkDialect,
                &expected,
                &existing,
                &AutogenerateOptions::default(),
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_matching_type_change_hint_produces_an_alter_column_type_on_postgres() {
        use crate::dialect::NumberedDialect;

        let expected = vec![table_spec(
            "users",
            vec![
                spec("id", ColumnType::I64, false),
                spec("age", ColumnType::I64, false),
            ],
            Some("id"),
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("id", false), live_column("age", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        let options = AutogenerateOptions {
            renamed_columns: Vec::new(),
            allow_drop_tables: Vec::new(),
            changed_column_types: vec![("users".to_string(), "age".to_string())],
        };

        let statements = diff(&NumberedDialect, &expected, &existing, &options);
        assert_eq!(
            statements,
            vec![
                r#"ALTER TABLE "users" ALTER COLUMN "age" TYPE BIGINT USING "age"::BIGINT"#
                    .to_string()
            ]
        );
    }

    #[test]
    fn an_unhinted_same_named_column_never_gets_a_type_change_even_on_postgres() {
        use crate::dialect::NumberedDialect;

        let expected = vec![table_spec(
            "users",
            vec![spec("age", ColumnType::I64, false)],
            None,
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("age", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );

        assert_eq!(
            diff(
                &NumberedDialect,
                &expected,
                &existing,
                &AutogenerateOptions::default(),
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_stale_type_change_hint_naming_a_column_missing_from_either_side_is_ignored() {
        use crate::dialect::NumberedDialect;

        let expected = vec![table_spec(
            "users",
            vec![spec("age", ColumnType::I64, false)],
            None,
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("age", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        // "nickname" isn't a column of "users" on either side.
        let options = AutogenerateOptions {
            renamed_columns: Vec::new(),
            allow_drop_tables: Vec::new(),
            changed_column_types: vec![("users".to_string(), "nickname".to_string())],
        };

        assert_eq!(
            diff(&NumberedDialect, &expected, &existing, &options),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_type_change_hint_is_ignored_on_a_dialect_that_cant_alter_a_columns_type_directly() {
        // The same hint that produces a statement on Postgres (see
        // `a_matching_type_change_hint_produces_an_alter_column_type_on_postgres`)
        // produces nothing at all on SQLite, which has no direct `ALTER
        // COLUMN ... TYPE` support — `diff` checks
        // `Dialect::supports_alter_column_type()` before ever building the
        // statement, rather than reaching `AlterTable::alter_column_type`'s
        // own panic for an unsupported dialect.
        let expected = vec![table_spec(
            "users",
            vec![spec("age", ColumnType::I64, false)],
            None,
        )];
        let mut existing = HashMap::new();
        existing.insert(
            "users".to_string(),
            TableSchema {
                name: "users".to_string(),
                columns: vec![live_column("age", false)],
                unique_constraints: Vec::new(),
                check_constraints: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
            },
        );
        let options = AutogenerateOptions {
            renamed_columns: Vec::new(),
            allow_drop_tables: Vec::new(),
            changed_column_types: vec![("users".to_string(), "age".to_string())],
        };

        assert_eq!(
            diff(&QuestionMarkDialect, &expected, &existing, &options),
            Vec::<String>::new()
        );
    }
}
