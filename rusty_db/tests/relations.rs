#![cfg(all(feature = "sqlite", feature = "derive"))]

use std::collections::HashMap;

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
#[has_many(Order, foreign_key = "user_id")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "orders")]
#[belongs_to(User, foreign_key = "user_id")]
struct Order {
    #[table(primary_key)]
    id: i64,
    user_id: i64,
    amount: i64,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, amount INTEGER NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn has_many_batches_children_for_a_batch_of_parents() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // A third user with no orders at all, to prove the map just has no entry for it.
    let linus = User {
        id: 3,
        name: "linus".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;
    engine.execute(&linus.insert()).await?;

    engine
        .execute(
            &(Order {
                id: 1,
                user_id: 1,
                amount: 100,
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Order {
                id: 2,
                user_id: 1,
                amount: 50,
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Order {
                id: 3,
                user_id: 2,
                amount: 200,
            })
            .insert(),
        )
        .await?;

    let users = vec![ada.clone(), grace.clone(), linus.clone()];
    let orders_by_user: HashMap<i64, Vec<Order>> = User::load_orders(&engine, &users).await?;

    assert_eq!(orders_by_user.len(), 2); // linus has no entry at all
    let mut ada_orders = orders_by_user.get(&ada.id).unwrap().clone();
    ada_orders.sort_by_key(|o| o.id);
    assert_eq!(
        ada_orders,
        vec![
            Order {
                id: 1,
                user_id: 1,
                amount: 100
            },
            Order {
                id: 2,
                user_id: 1,
                amount: 50
            },
        ]
    );
    assert_eq!(
        orders_by_user.get(&grace.id).unwrap(),
        &vec![Order {
            id: 3,
            user_id: 2,
            amount: 200
        }]
    );
    assert!(!orders_by_user.contains_key(&linus.id));

    Ok(())
}

#[tokio::test]
async fn belongs_to_batches_distinct_parents_for_a_batch_of_children() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;

    let orders = vec![
        Order {
            id: 1,
            user_id: 1,
            amount: 100,
        },
        Order {
            id: 2,
            user_id: 1,
            amount: 50,
        },
        Order {
            id: 3,
            user_id: 2,
            amount: 200,
        },
    ];
    for order in &orders {
        engine.execute(&order.insert()).await?;
    }

    let users_by_id: HashMap<i64, User> = Order::load_user(&engine, &orders).await?;

    // Two orders share user_id 1 -> the map still has exactly one entry for it.
    assert_eq!(users_by_id.len(), 2);
    assert_eq!(users_by_id.get(&1).unwrap(), &ada);
    assert_eq!(users_by_id.get(&2).unwrap(), &grace);

    Ok(())
}

#[tokio::test]
async fn eager_load_helpers_return_empty_maps_for_empty_input() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let no_users: Vec<User> = Vec::new();
    let orders_by_user: HashMap<i64, Vec<Order>> = User::load_orders(&engine, &no_users).await?;
    assert!(orders_by_user.is_empty());

    let no_orders: Vec<Order> = Vec::new();
    let users_by_id: HashMap<i64, User> = Order::load_user(&engine, &no_orders).await?;
    assert!(users_by_id.is_empty());

    Ok(())
}
