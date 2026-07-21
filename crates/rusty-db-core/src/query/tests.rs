use super::*;
use crate::dialect::{NumberedDialect, QuestionMarkDialect};
use crate::value::Value;

#[test]
fn select_renders_per_dialect_with_identical_builder_code() {
    let users = Table::new("users");
    let query = Select::from(&users)
        .columns([users.col("id"), users.col("name")])
        .filter(users.col("active").eq(true))
        .order_by(users.col("id").asc())
        .limit(5);

    let (sqlite_sql, sqlite_params) = query.to_sql(&QuestionMarkDialect);
    assert_eq!(
        sqlite_sql,
        r#"SELECT "users"."id", "users"."name" FROM "users" WHERE "users"."active" = ? ORDER BY "users"."id" ASC LIMIT 5"#
    );
    assert_eq!(sqlite_params, vec![Value::Bool(true)]);

    let (pg_sql, pg_params) = query.to_sql(&NumberedDialect);
    assert_eq!(
        pg_sql,
        r#"SELECT "users"."id", "users"."name" FROM "users" WHERE "users"."active" = $1 ORDER BY "users"."id" ASC LIMIT 5"#
    );
    assert_eq!(pg_params, vec![Value::Bool(true)]);
}

#[test]
fn compound_filters_render_with_parens_and_positional_params() {
    let users = Table::new("users");
    let query = Select::from(&users).filter(
        users
            .col("active")
            .eq(true)
            .and(users.col("age").gte(18_i64))
            .or(users.col("is_admin").eq(true)),
    );

    let (sql, params) = query.to_sql(&NumberedDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE (("users"."active" = $1) AND ("users"."age" >= $2)) OR ("users"."is_admin" = $3)"#
    );
    assert_eq!(
        params,
        vec![Value::Bool(true), Value::I64(18), Value::Bool(true)]
    );
}

#[test]
fn insert_only_adds_returning_when_dialect_supports_it() {
    let users = Table::new("users");
    let insert = Insert::into_table(&users)
        .value("name", "ada")
        .returning(["id"]);

    let (pg_sql, _) = insert.to_sql(&NumberedDialect);
    assert!(pg_sql.contains("RETURNING \"id\""));

    let (sqlite_sql, _) = insert.to_sql(&QuestionMarkDialect);
    assert!(!sqlite_sql.contains("RETURNING"));
}

#[test]
fn update_and_delete_render_where_clause() {
    let users = Table::new("users");

    let (update_sql, update_params) = Update::table(&users)
        .set("active", false)
        .filter(users.col("id").eq(1_i64))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        update_sql,
        r#"UPDATE "users" SET "active" = ? WHERE "users"."id" = ?"#
    );
    assert_eq!(update_params, vec![Value::Bool(false), Value::I64(1)]);

    let (delete_sql, delete_params) = Delete::from(&users)
        .filter(users.col("id").eq(1_i64))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(delete_sql, r#"DELETE FROM "users" WHERE "users"."id" = ?"#);
    assert_eq!(delete_params, vec![Value::I64(1)]);
}

#[test]
fn empty_in_list_is_always_false() {
    let users = Table::new("users");
    let (sql, params) = Select::from(&users)
        .filter(users.col("id").is_in(std::iter::empty()))
        .to_sql(&QuestionMarkDialect);
    assert!(sql.contains("1 = 0"));
    assert!(params.is_empty());
}
