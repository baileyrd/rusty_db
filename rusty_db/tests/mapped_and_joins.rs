#![cfg(all(feature = "sqlite", feature = "derive"))]

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "orders")]
struct Order {
    #[table(primary_key)]
    id: i64,
    user_id: i64,
    amount: i64,
}

#[tokio::test]
async fn mapped_struct_crud_and_joins() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;

    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
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

    let ada = User {
        id: 1,
        name: "ada".to_string(),
        active: true,
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
        active: false,
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;

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

    // `fetch_all_as` decodes rows straight into the mapped struct.
    let all_users: Vec<User> = engine
        .fetch_all_as(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    assert_eq!(all_users, vec![ada.clone(), grace.clone()]);

    // Struct-generated `update()` / `delete_query()`.
    let mut promoted = grace.clone();
    promoted.active = true;
    engine.execute(&promoted.update()).await?;

    let refetched: User = engine
        .fetch_one_as(&Select::from(&User::table()).filter(User::table().col("id").eq(promoted.id)))
        .await?;
    assert_eq!(refetched, promoted);

    engine.execute(&ada.delete_query()).await?;
    let remaining: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(remaining, vec![promoted.clone()]);

    // Join across the mapped tables' `Table` handles.
    let joined = engine
        .fetch_all(
            &Select::from(&Order::table())
                .columns([Order::table().col("amount"), User::table().col("name")])
                .join(
                    &User::table(),
                    Order::table()
                        .col("user_id")
                        .eq_col(&User::table().col("id")),
                )
                .filter(User::table().col("id").eq(promoted.id)),
        )
        .await?;

    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].get::<i64>(0)?, 200);
    assert_eq!(joined[0].get::<String>(1)?, "grace");

    Ok(())
}
