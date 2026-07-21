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
fn join_renders_kind_table_and_on_clause() {
    let users = Table::new("users");
    let orders = Table::new("orders");

    let query = Select::from(&orders)
        .columns([orders.col("id"), users.col("name")])
        .join(&users, orders.col("user_id").eq_col(&users.col("id")))
        .filter(users.col("active").eq(true));

    let (sql, params) = query.to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "orders"."id", "users"."name" FROM "orders" INNER JOIN "users" ON "orders"."user_id" = "users"."id" WHERE "users"."active" = ?"#
    );
    assert_eq!(params, vec![Value::Bool(true)]);
}

#[test]
fn left_join_renders_left_join_keyword() {
    let users = Table::new("users");
    let orders = Table::new("orders");

    let query =
        Select::from(&users).left_join(&orders, orders.col("user_id").eq_col(&users.col("id")));

    let (sql, _) = query.to_sql(&NumberedDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" LEFT JOIN "orders" ON "orders"."user_id" = "users"."id""#
    );
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

#[test]
fn distinct_adds_the_keyword_right_after_select() {
    let users = Table::new("users");

    let (sql, _) = Select::from(&users)
        .columns([users.col("name")])
        .distinct()
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT DISTINCT "users"."name" FROM "users""#);

    let (plain_sql, _) = Select::from(&users)
        .columns([users.col("name")])
        .to_sql(&QuestionMarkDialect);
    assert!(!plain_sql.contains("DISTINCT"));
}

#[test]
fn update_and_delete_only_add_returning_when_dialect_supports_it() {
    let users = Table::new("users");

    let update = Update::table(&users)
        .set("active", false)
        .filter(users.col("id").eq(1_i64))
        .returning(["id", "active"]);
    let (pg_sql, _) = update.clone().to_sql(&NumberedDialect);
    assert!(pg_sql.contains(r#"RETURNING "id", "active""#));
    let (sqlite_sql, _) = update.to_sql(&QuestionMarkDialect);
    assert!(!sqlite_sql.contains("RETURNING"));

    let delete = Delete::from(&users)
        .filter(users.col("id").eq(1_i64))
        .returning(["id"]);
    let (pg_sql, _) = delete.clone().to_sql(&NumberedDialect);
    assert!(pg_sql.contains(r#"RETURNING "id""#));
    let (sqlite_sql, _) = delete.to_sql(&QuestionMarkDialect);
    assert!(!sqlite_sql.contains("RETURNING"));
}

#[test]
fn between_renders_inclusive_bounds() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .filter(orders.col("amount").between(10_i64, 100_i64))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "orders" WHERE "orders"."amount" BETWEEN ? AND ?"#
    );
    assert_eq!(params, vec![Value::I64(10), Value::I64(100)]);
}

#[test]
fn table_alias_renders_as_clause_and_qualifies_its_own_columns() {
    let employees = Table::new("employees");
    let managers = employees.alias("managers");

    // The alias is a distinct `Table` handle: its own `.col(...)` qualifies
    // with the alias, while the original still qualifies with the real name.
    let query = Select::from(&employees)
        .columns([employees.col("name"), managers.col("name")])
        .join(
            &managers,
            employees.col("manager_id").eq_col(&managers.col("id")),
        );

    let (sql, _) = query.to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "employees"."name", "managers"."name" FROM "employees" INNER JOIN "employees" AS "managers" ON "employees"."manager_id" = "managers"."id""#
    );
}

#[test]
fn a_table_without_an_alias_renders_unchanged() {
    let users = Table::new("users");
    let (sql, _) = Select::from(&users).to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT * FROM "users""#);
}

#[test]
fn text_rewrites_question_mark_placeholders_per_dialect_in_order() {
    let users = Table::new("users");
    let query = Select::from(&users).filter(
        Expr::text(
            "lower(name) = ? AND age > ?",
            [Value::Text("ada".into()), Value::I64(18)],
        )
        .and(users.col("active").eq(true)),
    );

    let (sqlite_sql, sqlite_params) = query.clone().to_sql(&QuestionMarkDialect);
    assert_eq!(
        sqlite_sql,
        r#"SELECT * FROM "users" WHERE (lower(name) = ? AND age > ?) AND ("users"."active" = ?)"#
    );
    assert_eq!(
        sqlite_params,
        vec![Value::Text("ada".into()), Value::I64(18), Value::Bool(true)]
    );

    let (pg_sql, pg_params) = query.to_sql(&NumberedDialect);
    assert_eq!(
        pg_sql,
        r#"SELECT * FROM "users" WHERE (lower(name) = $1 AND age > $2) AND ("users"."active" = $3)"#
    );
    assert_eq!(
        pg_params,
        vec![Value::Text("ada".into()), Value::I64(18), Value::Bool(true)]
    );
}

#[test]
fn text_with_no_params_passes_through_unchanged() {
    let users = Table::new("users");
    let (sql, params) = Select::from(&users)
        .filter(Expr::text("1 = 1", []))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT * FROM "users" WHERE 1 = 1"#);
    assert!(params.is_empty());
}

#[test]
fn ilike_renders_as_ilike_on_postgres_and_falls_back_to_like_elsewhere() {
    let users = Table::new("users");
    let query = Select::from(&users).filter(users.col("name").ilike("%ada%"));

    let (pg_sql, _) = query.clone().to_sql(&NumberedDialect);
    assert!(pg_sql.contains(r#""users"."name" ILIKE $1"#));

    let (sqlite_sql, _) = query.to_sql(&QuestionMarkDialect);
    assert!(sqlite_sql.contains(r#""users"."name" LIKE ?"#));
}
