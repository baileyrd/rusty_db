#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[hybrid(name = "...", expr = "...")]`: a struct-level
//! attribute generating both a plain Rust method computing a value from
//! this instance's own fields, and a `_expr()` associated function
//! returning the same computation as a portable SQL `Expr`, usable in
//! `.filter()`/`.columns()` (anywhere else an `Expr` is accepted).

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "line_items")]
#[hybrid(name = "total", expr = "price * quantity")]
#[hybrid(
    name = "discounted_total",
    expr = "(price * quantity) - discount",
    ty = "i64"
)]
#[hybrid(name = "is_expensive", expr = "price > 50")]
struct LineItem {
    #[table(primary_key)]
    id: i64,
    price: i64,
    quantity: i64,
    discount: i64,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_hybrid_properties_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE line_items (id INTEGER PRIMARY KEY, price INTEGER NOT NULL, \
             quantity INTEGER NOT NULL, discount INTEGER NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

fn sample_items() -> Vec<LineItem> {
    vec![
        LineItem {
            id: 1,
            price: 10,
            quantity: 2,
            discount: 1,
        },
        LineItem {
            id: 2,
            price: 5,
            quantity: 5,
            discount: 0,
        },
        LineItem {
            id: 3,
            price: 100,
            quantity: 1,
            discount: 20,
        },
    ]
}

async fn seed(engine: &Engine, items: &[LineItem]) -> rusty_db::Result<()> {
    let bulk = BulkInsert::combine(items.iter().map(Entity::insert))?
        .expect("non-empty input produces a statement");
    engine.execute(&bulk).await?;
    Ok(())
}

#[tokio::test]
async fn the_rust_side_method_computes_the_same_arithmetic_as_the_expression_string(
) -> rusty_db::Result<()> {
    let item = LineItem {
        id: 1,
        price: 10,
        quantity: 3,
        discount: 4,
    };
    assert_eq!(item.total(), 30);
    assert_eq!(item.discounted_total(), 26);
    Ok(())
}

#[tokio::test]
async fn the_sql_side_expr_filters_rows_matching_the_rust_side_computation() -> rusty_db::Result<()>
{
    let engine = file_engine("filter").await?;
    let items = sample_items();
    seed(&engine, &items).await?;

    let rows: Vec<LineItem> = engine
        .fetch_all_as(&Select::from(&LineItem::table()).filter(LineItem::total_expr().gt(15_i64)))
        .await?;

    let expected: Vec<i64> = items
        .iter()
        .filter(|i| i.total() > 15)
        .map(|i| i.id)
        .collect();
    let mut actual: Vec<i64> = rows.iter().map(|i| i.id).collect();
    actual.sort();
    assert_eq!(actual, expected);

    Ok(())
}

#[tokio::test]
async fn the_sql_side_expr_works_as_a_select_column_and_matches_the_rust_side_value(
) -> rusty_db::Result<()> {
    let engine = file_engine("select_column").await?;
    let items = sample_items();
    seed(&engine, &items).await?;

    let rows = engine
        .fetch_all(
            &Select::from(&LineItem::table())
                .columns([
                    SelectExpr::from(LineItem::table().col("id")),
                    SelectExpr::from(LineItem::total_expr()).alias("computed_total"),
                ])
                .order_by(LineItem::table().col("id").asc()),
        )
        .await?;

    for (row, item) in rows.iter().zip(items.iter()) {
        let id: i64 = row.get_by_name("id")?;
        let computed_total: i64 = row.get_by_name("computed_total")?;
        assert_eq!(id, item.id);
        assert_eq!(computed_total, item.total());
    }

    Ok(())
}

#[tokio::test]
async fn an_explicit_ty_and_parenthesized_expression_are_both_honored() -> rusty_db::Result<()> {
    let engine = file_engine("explicit_ty").await?;
    let items = sample_items();
    seed(&engine, &items).await?;

    // (price * quantity) - discount, ty = "i64" explicitly.
    let rows: Vec<LineItem> = engine
        .fetch_all_as(
            &Select::from(&LineItem::table()).filter(LineItem::discounted_total_expr().gte(20_i64)),
        )
        .await?;

    let expected: Vec<i64> = items
        .iter()
        .filter(|i| i.discounted_total() >= 20)
        .map(|i| i.id)
        .collect();
    let mut actual: Vec<i64> = rows.iter().map(|i| i.id).collect();
    actual.sort();
    assert_eq!(actual, expected);

    Ok(())
}

#[tokio::test]
async fn a_comparison_hybrid_computes_a_bool_on_the_rust_side() -> rusty_db::Result<()> {
    let cheap = LineItem {
        id: 1,
        price: 10,
        quantity: 1,
        discount: 0,
    };
    let expensive = LineItem {
        id: 2,
        price: 100,
        quantity: 1,
        discount: 0,
    };
    assert!(!cheap.is_expensive());
    assert!(expensive.is_expensive());
    Ok(())
}

#[tokio::test]
async fn a_comparison_hybrids_sql_side_filters_the_same_rows_the_rust_side_would_keep(
) -> rusty_db::Result<()> {
    let engine = file_engine("comparison_filter").await?;
    let items = sample_items();
    seed(&engine, &items).await?;

    // is_expensive_expr() is already a complete boolean condition (unlike
    // total_expr(), a bare arithmetic Expr that still needs `.gt(...)`
    // applied) — it's used directly as the filter, no further comparison
    // chained onto it.
    let rows: Vec<LineItem> = engine
        .fetch_all_as(&Select::from(&LineItem::table()).filter(LineItem::is_expensive_expr()))
        .await?;

    let expected: Vec<i64> = items
        .iter()
        .filter(|i| i.is_expensive())
        .map(|i| i.id)
        .collect();
    let mut actual: Vec<i64> = rows.iter().map(|i| i.id).collect();
    actual.sort();
    assert_eq!(actual, expected);
    // The sample data actually has both an expensive and a non-expensive
    // item, so this test would catch a filter that's silently a no-op
    // (e.g. always true or always false) too.
    assert!(!expected.is_empty() && expected.len() < items.len());

    Ok(())
}
