//! Generates `#[derive(Mapped)]` struct source from a reflected
//! `TableSchema` (see `schema.rs`) — a starting point for mapping an
//! existing database's tables, not a fully automatic replacement for
//! hand-writing them: column type names are dialect-specific strings with
//! no standardized spelling across SQLite/Postgres/MySQL, so the Rust type
//! each one maps to is a best-effort heuristic (falling back to `String`
//! for anything unrecognized), and relationships (`has_many`/`belongs_to`/
//! etc.) aren't generated at all — detected foreign keys are only left as
//! a trailing comment, for the caller to wire up by hand.

use heck::ToUpperCamelCase;

use crate::schema::TableSchema;

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "async", "await", "try", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield",
];

/// A column name straight from a database catalog is almost always already
/// a valid Rust identifier, but isn't guaranteed to be — this covers the
/// exceptions (a leading digit, a character Rust identifiers don't allow,
/// or landing on a Rust keyword) rather than assuming the common case
/// always holds.
fn sanitize_field_name(column_name: &str) -> String {
    let mut name: String = column_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    match name.chars().next() {
        Some(c) if c.is_ascii_digit() => name = format!("_{name}"),
        None => name = "_".to_string(),
        _ => {}
    }
    if RUST_KEYWORDS.contains(&name.as_str()) {
        name = format!("r#{name}");
    }
    name
}

/// Best-effort mapping from a column's dialect-specific `type_name` (e.g.
/// Postgres's `"timestamp without time zone"`, MySQL's `"int(11)
/// unsigned"`, SQLite's `"INTEGER"`) to a Rust type usable in a
/// `#[derive(Mapped)]` field. Case-insensitive substring matching, since
/// none of the three dialects standardize on the same spelling — correct
/// for the common, standard column types; anything it doesn't recognize
/// falls back to `String`, safe but likely needing a manual fix-up.
fn rust_type_for(type_name: &str) -> &'static str {
    let t = type_name.to_ascii_lowercase();
    if t.contains("bool") {
        "bool"
    } else if t.contains("uuid") {
        "Uuid"
    } else if t.contains("json") {
        "Json"
    } else if t.contains("numeric") || t.contains("decimal") {
        "BigDecimal"
    } else if t.contains("blob") || t.contains("bytea") || t.contains("binary") {
        "Vec<u8>"
    } else if (t.contains("timestamp") || t.contains("datetime"))
        && (t.contains("tz") || t.contains("with time zone"))
    {
        "DateTime<Utc>"
    } else if t.contains("timestamp") || t.contains("datetime") {
        "NaiveDateTime"
    } else if t.contains("date") {
        "NaiveDate"
    } else if t.contains("time") {
        "NaiveTime"
    } else if t.contains("real") || t.contains("float") || t.contains("double") {
        "f64"
    } else if t.contains("int") {
        "i64"
    } else {
        "String"
    }
}

/// Generates `#[derive(Mapped)]` struct source for one reflected table —
/// paste the result into a `.rs` file (with `use rusty_db::prelude::*;` in
/// scope) as a starting point, then adjust: fix any field whose heuristic
/// type guess is wrong, add `#[table(version)]`/`#[table(soft_delete)]`
/// where relevant (schema reflection has no way to infer either), and wire
/// up any relationships from the foreign keys listed in the trailing
/// comment. See `Engine::automap_table`/`automap_all`.
pub fn generate_struct(schema: &TableSchema) -> String {
    let struct_name = schema.name.to_upper_camel_case();

    let mut out = String::new();
    out.push_str("#[derive(Mapped, Debug, Clone)]\n");
    out.push_str(&format!("#[table(name = \"{}\")]\n", schema.name));
    out.push_str(&format!("struct {struct_name} {{\n"));
    for column in &schema.columns {
        let field_name = sanitize_field_name(&column.name);
        let mut rust_type = rust_type_for(&column.type_name).to_string();
        if column.nullable && !column.primary_key {
            rust_type = format!("Option<{rust_type}>");
        }
        if column.primary_key {
            out.push_str("    #[table(primary_key)]\n");
        }
        out.push_str(&format!("    #[table(column = \"{}\")]\n", column.name));
        out.push_str(&format!("    {field_name}: {rust_type},\n"));
    }
    out.push_str("}\n");

    if !schema.foreign_keys.is_empty() {
        out.push_str("\n// Foreign keys detected on this table (not modeled as relationships\n");
        out.push_str("// above -- add has_many/belongs_to/etc. by hand if you want them):\n");
        for fk in &schema.foreign_keys {
            out.push_str(&format!(
                "// {}({}) -> {}({})\n",
                schema.name,
                fk.columns.join(", "),
                fk.referenced_table,
                fk.referenced_columns.join(", "),
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnInfo, ForeignKey};

    fn column(name: &str, type_name: &str, nullable: bool, primary_key: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            type_name: type_name.to_string(),
            nullable,
            primary_key,
            default: None,
        }
    }

    #[test]
    fn rust_type_for_maps_common_dialect_specific_type_names() {
        assert_eq!(rust_type_for("INTEGER"), "i64");
        assert_eq!(rust_type_for("bigint"), "i64");
        assert_eq!(rust_type_for("int(11) unsigned"), "i64");
        assert_eq!(rust_type_for("boolean"), "bool");
        assert_eq!(rust_type_for("text"), "String");
        assert_eq!(rust_type_for("character varying"), "String");
        assert_eq!(rust_type_for("real"), "f64");
        assert_eq!(rust_type_for("double precision"), "f64");
        assert_eq!(rust_type_for("numeric"), "BigDecimal");
        assert_eq!(rust_type_for("decimal(10,2)"), "BigDecimal");
        assert_eq!(rust_type_for("uuid"), "Uuid");
        assert_eq!(rust_type_for("json"), "Json");
        assert_eq!(rust_type_for("jsonb"), "Json");
        assert_eq!(rust_type_for("blob"), "Vec<u8>");
        assert_eq!(rust_type_for("bytea"), "Vec<u8>");
        assert_eq!(rust_type_for("date"), "NaiveDate");
        assert_eq!(rust_type_for("time"), "NaiveTime");
        assert_eq!(rust_type_for("datetime"), "NaiveDateTime");
        assert_eq!(
            rust_type_for("timestamp without time zone"),
            "NaiveDateTime"
        );
        assert_eq!(rust_type_for("timestamp with time zone"), "DateTime<Utc>");
        assert_eq!(rust_type_for("timestamptz"), "DateTime<Utc>");
        assert_eq!(rust_type_for("some_unrecognized_type"), "String");
    }

    #[test]
    fn sanitize_field_name_escapes_keywords_and_invalid_characters() {
        assert_eq!(sanitize_field_name("id"), "id");
        assert_eq!(sanitize_field_name("type"), "r#type");
        assert_eq!(sanitize_field_name("2fa_enabled"), "_2fa_enabled");
        assert_eq!(sanitize_field_name("first name"), "first_name");
    }

    #[test]
    fn generate_struct_renders_the_struct_header_and_every_column() {
        let schema = TableSchema {
            name: "orders".to_string(),
            columns: vec![
                column("id", "INTEGER", false, true),
                column("customer", "TEXT", false, false),
                column("nickname", "TEXT", true, false),
                column("type", "TEXT", false, false),
            ],
            unique_constraints: vec![],
            check_constraints: vec![],
            foreign_keys: vec![],
            indexes: vec![],
        };

        let source = generate_struct(&schema);
        assert!(source.contains("#[derive(Mapped, Debug, Clone)]"));
        assert!(source.contains("#[table(name = \"orders\")]"));
        assert!(source.contains("struct Orders {"));
        assert!(source.contains("#[table(primary_key)]"));
        assert!(source.contains("#[table(column = \"id\")]\n    id: i64,"));
        assert!(source.contains("#[table(column = \"customer\")]\n    customer: String,"));
        assert!(
            source.contains("#[table(column = \"nickname\")]\n    nickname: Option<String>,"),
            "a nullable, non-primary-key column should be wrapped in Option<T>"
        );
        assert!(
            source.contains("#[table(column = \"type\")]\n    r#type: String,"),
            "a column named after a Rust keyword should use a raw identifier"
        );
    }

    #[test]
    fn generate_struct_lists_foreign_keys_as_a_trailing_comment_not_a_relationship_field() {
        let schema = TableSchema {
            name: "orders".to_string(),
            columns: vec![column("id", "INTEGER", false, true)],
            unique_constraints: vec![],
            check_constraints: vec![],
            foreign_keys: vec![ForeignKey {
                name: "fk_customer".to_string(),
                columns: vec!["customer_id".to_string()],
                referenced_table: "customers".to_string(),
                referenced_columns: vec!["id".to_string()],
            }],
            indexes: vec![],
        };

        let source = generate_struct(&schema);
        assert!(source.contains("// orders(customer_id) -> customers(id)"));
        assert!(
            !source.contains("#[relation")
                && !source.contains("has_many(")
                && !source.contains("belongs_to("),
            "relationships should only be listed as a comment, not generated as actual code"
        );
    }
}
