use super::*;
use crate::dialect::{MySqlDialect, NumberedDialect, QuestionMarkDialect};
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

#[test]
fn count_all_renders_as_count_star_with_no_params() {
    let users = Table::new("users");
    let (sql, params) = Select::from(&users)
        .columns([SelectExpr::from(Expr::count_all())])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT COUNT(*) FROM "users""#);
    assert!(params.is_empty());
}

#[test]
fn aggregate_over_a_column_renders_and_can_be_aliased() {
    let orders = Table::new("orders");
    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(orders.col("amount").sum()).alias("total")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT SUM("orders"."amount") AS "total" FROM "orders""#
    );

    // Every aggregate this crate supports, unaliased.
    let (sql, _) = Select::from(&orders)
        .columns([
            SelectExpr::from(orders.col("amount").count()),
            SelectExpr::from(orders.col("amount").avg()),
            SelectExpr::from(orders.col("amount").min()),
            SelectExpr::from(orders.col("amount").max()),
        ])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT COUNT("orders"."amount"), AVG("orders"."amount"), MIN("orders"."amount"), MAX("orders"."amount") FROM "orders""#
    );
}

#[test]
fn plain_and_expression_columns_compose_in_one_select_via_select_expr() {
    let orders = Table::new("orders");
    let (sql, _) = Select::from(&orders)
        .columns([
            SelectExpr::from(orders.col("user_id")),
            SelectExpr::from(orders.col("amount").sum()).alias("total"),
        ])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "orders"."user_id", SUM("orders"."amount") AS "total" FROM "orders""#
    );
}

#[test]
fn arbitrary_expression_can_be_a_select_column() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .columns([SelectExpr::from(Expr::text("amount * ?", [Value::F64(1.1)])).alias("with_tax")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT amount * ? AS "with_tax" FROM "orders""#);
    assert_eq!(params, vec![Value::F64(1.1)]);
}

#[test]
fn group_by_renders_after_where_and_before_order_by() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .columns([
            SelectExpr::from(orders.col("customer")),
            SelectExpr::from(orders.col("amount").sum()).alias("total"),
        ])
        .filter(orders.col("amount").gt(0_i64))
        .group_by([orders.col("customer")])
        .order_by(orders.col("customer").asc())
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "orders"."customer", SUM("orders"."amount") AS "total" FROM "orders" WHERE "orders"."amount" > ? GROUP BY "orders"."customer" ORDER BY "orders"."customer" ASC"#
    );
    assert_eq!(params, vec![Value::I64(0)]);
}

#[test]
fn group_by_accepts_multiple_columns() {
    let orders = Table::new("orders");
    let (sql, _) = Select::from(&orders)
        .group_by([orders.col("customer"), orders.col("status")])
        .to_sql(&QuestionMarkDialect);
    assert!(sql.contains(r#"GROUP BY "orders"."customer", "orders"."status""#));
}

#[test]
fn having_filters_on_an_aggregate_and_combines_with_and_on_repeated_calls() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .columns([
            SelectExpr::from(orders.col("customer")),
            SelectExpr::from(orders.col("amount").sum()).alias("total"),
        ])
        .group_by([orders.col("customer")])
        .having(orders.col("amount").sum().gt(100_i64))
        .having(orders.col("amount").count().lt(10_i64))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "orders"."customer", SUM("orders"."amount") AS "total" FROM "orders" GROUP BY "orders"."customer" HAVING (SUM("orders"."amount") > ?) AND (COUNT("orders"."amount") < ?)"#
    );
    assert_eq!(params, vec![Value::I64(100), Value::I64(10)]);
}

#[test]
fn expr_level_comparisons_work_on_an_arbitrary_expression_not_just_a_column() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .filter(Expr::text("amount * 2", []).gte(50_i64))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT * FROM "orders" WHERE amount * 2 >= ?"#);
    assert_eq!(params, vec![Value::I64(50)]);
}

#[test]
fn union_renders_both_arms_joined_by_the_keyword() {
    let active = Table::new("active_users");
    let archived = Table::new("archived_users");

    let (sql, _) = Select::from(&active)
        .columns([active.col("id")])
        .union(Select::from(&archived).columns([archived.col("id")]))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "active_users"."id" FROM "active_users" UNION SELECT "archived_users"."id" FROM "archived_users""#
    );
}

#[test]
fn union_all_intersect_and_except_render_their_own_keyword() {
    let a = Table::new("a");
    let b = Table::new("b");

    let (union_all_sql, _) = Select::from(&a)
        .union_all(Select::from(&b))
        .to_sql(&QuestionMarkDialect);
    assert!(union_all_sql.contains(" UNION ALL "));

    let (intersect_sql, _) = Select::from(&a)
        .intersect(Select::from(&b))
        .to_sql(&QuestionMarkDialect);
    assert!(intersect_sql.contains(" INTERSECT "));

    let (except_sql, _) = Select::from(&a)
        .except(Select::from(&b))
        .to_sql(&QuestionMarkDialect);
    assert!(except_sql.contains(" EXCEPT "));
}

#[test]
fn set_operations_chain_to_combine_more_than_two_selects() {
    let a = Table::new("a");
    let b = Table::new("b");
    let c = Table::new("c");

    let (sql, _) = Select::from(&a)
        .union(Select::from(&b))
        .union_all(Select::from(&c))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "a" UNION SELECT * FROM "b" UNION ALL SELECT * FROM "c""#
    );
}

#[test]
fn set_operation_params_are_numbered_sequentially_across_both_arms_on_postgres() {
    let orders = Table::new("orders");
    let refunds = Table::new("refunds");

    let (sql, params) = Select::from(&orders)
        .columns([orders.col("id")])
        .filter(orders.col("amount").gt(10_i64))
        .union(
            Select::from(&refunds)
                .columns([refunds.col("id")])
                .filter(refunds.col("amount").gt(20_i64)),
        )
        .to_sql(&NumberedDialect);
    // Each arm renders its own placeholder independently via Select::to_sql,
    // but SetOperation threads one shared params list through both arms via
    // render_into, so the second arm's placeholder continues as $2, not a
    // colliding, restarted $1.
    assert_eq!(
        sql,
        r#"SELECT "orders"."id" FROM "orders" WHERE "orders"."amount" > $1 UNION SELECT "refunds"."id" FROM "refunds" WHERE "refunds"."amount" > $2"#
    );
    assert_eq!(params, vec![Value::I64(10), Value::I64(20)]);
}

#[test]
fn lower_and_upper_render_as_function_calls() {
    let users = Table::new("users");
    let (sql, _) = Select::from(&users)
        .filter(users.col("name").lower().eq("ada"))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE LOWER("users"."name") = ?"#
    );

    let (sql, _) = Select::from(&users)
        .filter(users.col("name").upper().eq("ADA"))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE UPPER("users"."name") = ?"#
    );
}

#[test]
fn concat_renders_double_pipe_on_postgres_and_sqlite_but_concat_call_on_mysql() {
    let users = Table::new("users");
    let query = Select::from(&users).columns([SelectExpr::from(
        users
            .col("first_name")
            .concat(Expr::lit(" "))
            .concat(Expr::col(users.col("last_name"))),
    )
    .alias("full_name")]);

    let (pg_sql, _) = query.clone().to_sql(&NumberedDialect);
    assert_eq!(
        pg_sql,
        r#"SELECT "users"."first_name" || $1 || "users"."last_name" AS "full_name" FROM "users""#
    );

    let (sqlite_sql, _) = query.clone().to_sql(&QuestionMarkDialect);
    assert!(sqlite_sql.contains(r#""users"."first_name" || ? || "users"."last_name""#));

    let (mysql_sql, _) = query.to_sql(&MySqlDialect);
    assert!(mysql_sql
        .contains("CONCAT(CONCAT(`users`.`first_name`, ?), `users`.`last_name`) AS `full_name`"));
}

#[test]
fn arithmetic_operators_render_correctly() {
    let orders = Table::new("orders");
    let (sql, params) = Select::from(&orders)
        .columns([SelectExpr::from(orders.col("amount").mul(Expr::lit(1.1_f64))).alias("with_tax")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "orders"."amount" * ? AS "with_tax" FROM "orders""#
    );
    assert_eq!(params, vec![Value::F64(1.1)]);

    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            orders.col("amount").add(Expr::col(orders.col("tax"))),
        )])
        .to_sql(&QuestionMarkDialect);
    assert!(sql.contains(r#""orders"."amount" + "orders"."tax""#));

    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            orders.col("amount").sub(Expr::col(orders.col("discount"))),
        )])
        .to_sql(&QuestionMarkDialect);
    assert!(sql.contains(r#""orders"."amount" - "orders"."discount""#));

    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            orders.col("total").div(Expr::col(orders.col("count"))),
        )])
        .to_sql(&QuestionMarkDialect);
    assert!(sql.contains(r#""orders"."total" / "orders"."count""#));
}

#[test]
fn now_renders_as_current_timestamp_on_every_dialect() {
    let events = Table::new("events");
    let (sql, _) = Select::from(&events)
        .columns([SelectExpr::from(Expr::now()).alias("now")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT CURRENT_TIMESTAMP AS "now" FROM "events""#);
}

#[test]
fn expr_to_expr_comparisons_let_now_be_used_directly_in_a_filter() {
    let events = Table::new("events");
    let (sql, _) = Select::from(&events)
        .filter(Expr::col(events.col("created_at")).lt_expr(Expr::now()))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "events" WHERE "events"."created_at" < CURRENT_TIMESTAMP"#
    );
}

#[test]
fn coalesce_renders_every_argument() {
    let users = Table::new("users");
    let (sql, _) = Select::from(&users)
        .columns([SelectExpr::from(Expr::coalesce([
            Expr::col(users.col("nickname")),
            Expr::col(users.col("name")),
            Expr::lit("anonymous"),
        ]))
        .alias("display_name")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT COALESCE("users"."nickname", "users"."name", ?) AS "display_name" FROM "users""#
    );
}

#[test]
fn case_renders_every_arm_and_the_else_clause() {
    let orders = Table::new("orders");
    let tier = Case::new()
        .when(orders.col("amount").gt(100_i64), Expr::lit("gold"))
        .when(orders.col("amount").gt(50_i64), Expr::lit("silver"))
        .otherwise(Expr::lit("bronze"));

    let (sql, params) = Select::from(&orders)
        .columns([SelectExpr::from(tier).alias("tier")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT CASE WHEN "orders"."amount" > ? THEN ? WHEN "orders"."amount" > ? THEN ? ELSE ? END AS "tier" FROM "orders""#
    );
    assert_eq!(
        params,
        vec![
            Value::I64(100),
            Value::Text("gold".to_string()),
            Value::I64(50),
            Value::Text("silver".to_string()),
            Value::Text("bronze".to_string()),
        ]
    );
}

#[test]
fn case_without_an_else_clause_omits_it() {
    let orders = Table::new("orders");
    let status = Case::new().when(orders.col("amount").gt(0_i64), Expr::lit("has_amount"));

    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(status)])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT CASE WHEN "orders"."amount" > ? THEN ? END FROM "orders""#
    );
}

#[test]
fn in_subquery_renders_a_nested_select_and_numbers_params_sequentially_on_postgres() {
    let orders = Table::new("orders");
    let users = Table::new("users");
    let big_spenders = Select::from(&orders)
        .columns([orders.col("user_id")])
        .filter(orders.col("amount").gt(100_i64));

    let query = Select::from(&users)
        .filter(users.col("active").eq(true))
        .filter(users.col("id").in_subquery(big_spenders));

    let (sql, params) = query.to_sql(&NumberedDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE ("users"."active" = $1) AND ("users"."id" IN (SELECT "orders"."user_id" FROM "orders" WHERE "orders"."amount" > $2))"#
    );
    assert_eq!(params, vec![Value::Bool(true), Value::I64(100)]);
}

#[test]
fn exists_renders_a_correlated_subquery() {
    let orders = Table::new("orders");
    let users = Table::new("users");
    let has_orders = Select::from(&orders).filter(orders.col("user_id").eq_col(&users.col("id")));

    let (sql, _) = Select::from(&users)
        .filter(Expr::exists(has_orders))
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE EXISTS (SELECT * FROM "orders" WHERE "orders"."user_id" = "users"."id")"#
    );
}

#[test]
fn not_wrapping_exists_renders_not_exists_semantics() {
    let orders = Table::new("orders");
    let users = Table::new("users");
    let has_orders = Select::from(&orders).filter(orders.col("user_id").eq_col(&users.col("id")));

    let (sql, _) = Select::from(&users)
        .filter(Expr::exists(has_orders).not())
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE NOT (EXISTS (SELECT * FROM "orders" WHERE "orders"."user_id" = "users"."id"))"#
    );
}

#[test]
fn with_prefixes_a_named_cte_and_it_can_be_queried_by_name() {
    let orders = Table::new("orders");
    let big_orders = Cte::new(
        "big_orders",
        Select::from(&orders)
            .columns([orders.col("id")])
            .filter(orders.col("amount").gt(100_i64)),
    );

    let cte_ref = Table::new("big_orders");
    let (sql, params) = Select::from(&cte_ref)
        .with([big_orders])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"WITH "big_orders" AS (SELECT "orders"."id" FROM "orders" WHERE "orders"."amount" > ?) SELECT * FROM "big_orders""#
    );
    assert_eq!(params, vec![Value::I64(100)]);
}

#[test]
fn with_recursive_renders_the_recursive_keyword_and_the_anchor_union_recursive_term() {
    let employees = Table::new("employees");
    let org_chart = Table::new("org_chart");

    let anchor = Select::from(&employees)
        .columns([employees.col("id")])
        .filter(employees.col("manager_id").is_null());
    let recursive_term = Select::from(&employees)
        .columns([employees.col("id")])
        .join(
            &org_chart,
            employees.col("manager_id").eq_col(&org_chart.col("id")),
        );
    let cte = Cte::recursive_union_all("org_chart", anchor, recursive_term);

    let (sql, _) = Select::from(&org_chart)
        .with_recursive([cte])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"WITH RECURSIVE "org_chart" AS (SELECT "employees"."id" FROM "employees" WHERE "employees"."manager_id" IS NULL UNION ALL SELECT "employees"."id" FROM "employees" INNER JOIN "org_chart" ON "employees"."manager_id" = "org_chart"."id") SELECT * FROM "org_chart""#
    );
}

#[test]
fn recursive_union_without_all_dedupes_via_plain_union() {
    let employees = Table::new("employees");
    let org_chart = Table::new("org_chart");

    let anchor = Select::from(&employees).filter(employees.col("manager_id").is_null());
    let recursive_term = Select::from(&employees).join(
        &org_chart,
        employees.col("manager_id").eq_col(&org_chart.col("id")),
    );
    let cte = Cte::recursive_union("org_chart", anchor, recursive_term);

    let (sql, _) = Select::from(&org_chart)
        .with_recursive([cte])
        .to_sql(&QuestionMarkDialect);
    assert!(
        sql.contains(" UNION SELECT "),
        "expected plain UNION, not UNION ALL: {sql}"
    );
}

#[test]
fn with_clause_and_outer_query_bind_params_are_numbered_sequentially_on_postgres() {
    let orders = Table::new("orders");
    let big_orders = Cte::new(
        "big_orders",
        Select::from(&orders)
            .columns([orders.col("id")])
            .filter(orders.col("amount").gt(100_i64)),
    );

    let cte_ref = Table::new("big_orders");
    let (sql, params) = Select::from(&cte_ref)
        .with([big_orders])
        .filter(cte_ref.col("id").lt(1000_i64))
        .to_sql(&NumberedDialect);
    assert_eq!(
        sql,
        r#"WITH "big_orders" AS (SELECT "orders"."id" FROM "orders" WHERE "orders"."amount" > $1) SELECT * FROM "big_orders" WHERE "big_orders"."id" < $2"#
    );
    assert_eq!(params, vec![Value::I64(100), Value::I64(1000)]);
}

#[test]
fn scalar_subquery_renders_as_a_parenthesized_select_and_can_be_aliased() {
    let orders = Table::new("orders");
    let users = Table::new("users");
    let order_count = Select::from(&orders)
        .columns([SelectExpr::from(Expr::count_all())])
        .filter(orders.col("user_id").eq_col(&users.col("id")));

    let (sql, _) = Select::from(&users)
        .columns([
            SelectExpr::from(users.col("id")),
            SelectExpr::from(Expr::subquery(order_count)).alias("order_count"),
        ])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT "users"."id", (SELECT COUNT(*) FROM "orders" WHERE "orders"."user_id" = "users"."id") AS "order_count" FROM "users""#
    );
}

#[test]
fn row_number_renders_with_partition_by_and_order_by() {
    let orders = Table::new("orders");
    let row_num = Expr::row_number().over(
        Window::new()
            .partition_by([orders.col("customer")])
            .order_by(orders.col("id").asc()),
    );

    let (sql, params) = Select::from(&orders)
        .columns([SelectExpr::from(row_num).alias("rn")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT ROW_NUMBER() OVER (PARTITION BY "orders"."customer" ORDER BY "orders"."id" ASC) AS "rn" FROM "orders""#
    );
    assert!(params.is_empty());
}

#[test]
fn rank_and_dense_rank_render_their_own_function_name() {
    let orders = Table::new("orders");

    let (rank_sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            Expr::rank().over(Window::new().order_by(orders.col("amount").desc())),
        )
        .alias("r")])
        .to_sql(&QuestionMarkDialect);
    assert!(rank_sql.contains("RANK() OVER (ORDER BY \"orders\".\"amount\" DESC)"));

    let (dense_rank_sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            Expr::dense_rank().over(Window::new().order_by(orders.col("amount").desc())),
        )
        .alias("dr")])
        .to_sql(&QuestionMarkDialect);
    assert!(dense_rank_sql.contains("DENSE_RANK() OVER (ORDER BY \"orders\".\"amount\" DESC)"));
}

#[test]
fn an_aggregate_can_be_used_as_a_window_function() {
    let orders = Table::new("orders");
    let running_total = orders.col("amount").sum().over(
        Window::new()
            .partition_by([orders.col("customer")])
            .order_by(orders.col("id").asc()),
    );

    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(running_total).alias("running_total")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT SUM("orders"."amount") OVER (PARTITION BY "orders"."customer" ORDER BY "orders"."id" ASC) AS "running_total" FROM "orders""#
    );
}

#[test]
fn window_with_partition_by_only_omits_order_by() {
    let orders = Table::new("orders");
    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(
            Expr::count_all().over(Window::new().partition_by([orders.col("customer")])),
        )
        .alias("n")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(
        sql,
        r#"SELECT COUNT(*) OVER (PARTITION BY "orders"."customer") AS "n" FROM "orders""#
    );
}

#[test]
fn window_with_neither_clause_renders_an_empty_over() {
    let orders = Table::new("orders");
    let (sql, _) = Select::from(&orders)
        .columns([SelectExpr::from(Expr::count_all().over(Window::new())).alias("n")])
        .to_sql(&QuestionMarkDialect);
    assert_eq!(sql, r#"SELECT COUNT(*) OVER () AS "n" FROM "orders""#);
}
