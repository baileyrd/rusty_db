#![cfg(all(feature = "sqlite", feature = "derive"))]

use std::collections::HashMap;

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
#[has_many(Order, foreign_key = "user_id", cascade = "delete")]
#[has_one(Profile, foreign_key = "user_id", cascade = "delete")]
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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "profiles")]
struct Profile {
    #[table(primary_key)]
    id: i64,
    user_id: i64,
    bio: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "posts")]
#[many_to_many(
    Tag,
    through = "post_tags",
    local_key = "post_id",
    foreign_key = "tag_id",
    cascade = "delete"
)]
struct Post {
    #[table(primary_key)]
    id: i64,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "tags")]
struct Tag {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "teams")]
#[has_many(Player, foreign_key = "team_id", cascade = "orphan")]
struct Team {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "players")]
struct Player {
    #[table(primary_key)]
    id: i64,
    team_id: Option<i64>,
    name: String,
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
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE profiles (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, bio TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE post_tags (post_id INTEGER NOT NULL, tag_id INTEGER NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE teams (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE players (id INTEGER PRIMARY KEY, team_id INTEGER, name TEXT NOT NULL)",
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

    let no_profile_users: Vec<User> = Vec::new();
    let profiles_by_user: HashMap<i64, Profile> =
        User::load_profile(&engine, &no_profile_users).await?;
    assert!(profiles_by_user.is_empty());

    Ok(())
}

#[tokio::test]
async fn has_one_batches_at_most_one_child_per_parent() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // A third user with no profile at all, to prove the map just has no entry for it.
    let linus = User {
        id: 3,
        name: "linus".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;
    engine.execute(&linus.insert()).await?;

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let users = vec![ada.clone(), grace.clone(), linus.clone()];
    let profiles_by_user: HashMap<i64, Profile> = User::load_profile(&engine, &users).await?;

    assert_eq!(profiles_by_user.len(), 1); // grace and linus have no entry at all
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);
    assert!(!profiles_by_user.contains_key(&grace.id));
    assert!(!profiles_by_user.contains_key(&linus.id));

    Ok(())
}

#[tokio::test]
async fn has_one_reports_a_conflict_when_a_parent_has_more_than_one_matching_row(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    engine.execute(&ada.insert()).await?;

    // Two profiles for the same user: not actually a one-to-one relationship.
    engine
        .execute(
            &(Profile {
                id: 1,
                user_id: 1,
                bio: "mathematician".to_string(),
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Profile {
                id: 2,
                user_id: 1,
                bio: "also a mathematician".to_string(),
            })
            .insert(),
        )
        .await?;

    let result = User::load_profile(&engine, std::slice::from_ref(&ada)).await;
    assert!(matches!(result, Err(rusty_db::Error::Conflict(_))));

    Ok(())
}

async fn insert_post_tag(engine: &Engine, post_id: i64, tag_id: i64) -> rusty_db::Result<()> {
    let post_tags = Table::new("post_tags");
    engine
        .execute(
            &Insert::into_table(&post_tags)
                .value("post_id", post_id)
                .value("tag_id", tag_id),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn many_to_many_batches_targets_joined_through_a_join_table() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    let db_post = Post {
        id: 2,
        title: "Databases 101".to_string(),
    };
    // A third post with no tags at all, to prove the map just has no entry for it.
    let untagged_post = Post {
        id: 3,
        title: "Untagged".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    engine.execute(&db_post.insert()).await?;
    engine.execute(&untagged_post.insert()).await?;

    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    let db_tag = Tag {
        id: 2,
        name: "databases".to_string(),
    };
    let systems_tag = Tag {
        id: 3,
        name: "systems".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    engine.execute(&db_tag.insert()).await?;
    engine.execute(&systems_tag.insert()).await?;

    // rust_post: rust + systems: db_post: rust + databases.
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, rust_post.id, systems_tag.id).await?;
    insert_post_tag(&engine, db_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, db_post.id, db_tag.id).await?;

    let posts = vec![rust_post.clone(), db_post.clone(), untagged_post.clone()];
    let tags_by_post: HashMap<i64, Vec<Tag>> = Post::load_tags(&engine, &posts).await?;

    assert_eq!(tags_by_post.len(), 2); // untagged_post has no entry at all
    let mut rust_post_tags = tags_by_post.get(&rust_post.id).unwrap().clone();
    rust_post_tags.sort_by_key(|t| t.id);
    assert_eq!(rust_post_tags, vec![rust_tag.clone(), systems_tag.clone()]);
    let mut db_post_tags = tags_by_post.get(&db_post.id).unwrap().clone();
    db_post_tags.sort_by_key(|t| t.id);
    assert_eq!(db_post_tags, vec![rust_tag.clone(), db_tag.clone()]);
    assert!(!tags_by_post.contains_key(&untagged_post.id));

    Ok(())
}

#[tokio::test]
async fn many_to_many_returns_empty_map_for_empty_input() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let no_posts: Vec<Post> = Vec::new();
    let tags_by_post: HashMap<i64, Vec<Tag>> = Post::load_tags(&engine, &no_posts).await?;
    assert!(tags_by_post.is_empty());

    Ok(())
}

#[tokio::test]
async fn has_many_via_subquery_matches_the_select_in_result_for_a_filtered_parent_batch(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // Excluded from the parent batch below by its own filter, not by lack of orders.
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
    engine
        .execute(
            &(Order {
                id: 4,
                user_id: 3,
                amount: 999,
            })
            .insert(),
        )
        .await?;

    let users_table = User::table();
    // Only ada and grace are in the parent batch — linus is filtered out here,
    // not because linus has no orders (linus does).
    let parent_ids = Select::from(&users_table)
        .columns([users_table.col("id")])
        .filter(users_table.col("id").lt(3_i64));

    let orders_by_user: HashMap<i64, Vec<Order>> =
        rusty_db::relations::load_many_via_subquery(&engine, parent_ids, "id", "user_id").await?;

    assert_eq!(orders_by_user.len(), 2);
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
async fn has_one_via_subquery_matches_the_select_in_result() -> rusty_db::Result<()> {
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

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let users_table = User::table();
    let parent_ids = Select::from(&users_table).columns([users_table.col("id")]);

    let profiles_by_user: HashMap<i64, Profile> =
        rusty_db::relations::load_has_one_via_subquery(&engine, parent_ids, "id", "user_id")
            .await?;

    assert_eq!(profiles_by_user.len(), 1);
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);
    assert!(!profiles_by_user.contains_key(&grace.id));

    Ok(())
}

#[tokio::test]
async fn has_one_via_subquery_reports_the_same_conflict_as_the_select_in_version(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine
        .execute(
            &(Profile {
                id: 1,
                user_id: 1,
                bio: "mathematician".to_string(),
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Profile {
                id: 2,
                user_id: 1,
                bio: "also a mathematician".to_string(),
            })
            .insert(),
        )
        .await?;

    let users_table = User::table();
    let parent_ids = Select::from(&users_table).columns([users_table.col("id")]);

    let result: rusty_db::Result<HashMap<i64, Profile>> =
        rusty_db::relations::load_has_one_via_subquery(&engine, parent_ids, "id", "user_id").await;
    assert!(matches!(result, Err(rusty_db::Error::Conflict(_))));

    Ok(())
}

#[tokio::test]
async fn belongs_to_via_subquery_matches_the_select_in_result() -> rusty_db::Result<()> {
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

    let orders_table = Order::table();
    let foreign_key_ids = Select::from(&orders_table).columns([orders_table.col("user_id")]);

    let users_by_id: HashMap<i64, User> =
        rusty_db::relations::load_one_via_subquery(&engine, foreign_key_ids, "user_id", "id")
            .await?;

    assert_eq!(users_by_id.len(), 2);
    assert_eq!(users_by_id.get(&1).unwrap(), &ada);
    assert_eq!(users_by_id.get(&2).unwrap(), &grace);

    Ok(())
}

#[tokio::test]
async fn many_to_many_via_subquery_matches_the_select_in_result() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    let db_post = Post {
        id: 2,
        title: "Databases 101".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    engine.execute(&db_post.insert()).await?;

    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    let db_tag = Tag {
        id: 2,
        name: "databases".to_string(),
    };
    let systems_tag = Tag {
        id: 3,
        name: "systems".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    engine.execute(&db_tag.insert()).await?;
    engine.execute(&systems_tag.insert()).await?;

    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, rust_post.id, systems_tag.id).await?;
    insert_post_tag(&engine, db_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, db_post.id, db_tag.id).await?;

    let posts_table = Post::table();
    let parent_ids = Select::from(&posts_table).columns([posts_table.col("id")]);

    let tags_by_post: HashMap<i64, Vec<Tag>> = rusty_db::relations::load_many_to_many_via_subquery(
        &engine,
        parent_ids,
        "id",
        "post_tags",
        "post_id",
        "tag_id",
        "id",
    )
    .await?;

    assert_eq!(tags_by_post.len(), 2);
    let mut rust_post_tags = tags_by_post.get(&rust_post.id).unwrap().clone();
    rust_post_tags.sort_by_key(|t| t.id);
    assert_eq!(rust_post_tags, vec![rust_tag.clone(), systems_tag.clone()]);
    let mut db_post_tags = tags_by_post.get(&db_post.id).unwrap().clone();
    db_post_tags.sort_by_key(|t| t.id);
    assert_eq!(db_post_tags, vec![rust_tag.clone(), db_tag.clone()]);

    Ok(())
}

#[tokio::test]
async fn derive_generated_via_subquery_methods_work_end_to_end() -> rusty_db::Result<()> {
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

    let ada_order = Order {
        id: 1,
        user_id: 1,
        amount: 100,
    };
    let grace_order = Order {
        id: 2,
        user_id: 2,
        amount: 200,
    };
    engine.execute(&ada_order.insert()).await?;
    engine.execute(&grace_order.insert()).await?;

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;

    // has_many: User::load_orders_via_subquery.
    let users_table = User::table();
    let parent_ids = Select::from(&users_table).columns([users_table.col("id")]);
    let orders_by_user: HashMap<i64, Vec<Order>> =
        User::load_orders_via_subquery(&engine, parent_ids).await?;
    assert_eq!(
        orders_by_user.get(&ada.id).unwrap(),
        &vec![ada_order.clone()]
    );
    assert_eq!(
        orders_by_user.get(&grace.id).unwrap(),
        &vec![grace_order.clone()]
    );

    // has_one: User::load_profile_via_subquery.
    let parent_ids = Select::from(&users_table).columns([users_table.col("id")]);
    let profiles_by_user: HashMap<i64, Profile> =
        User::load_profile_via_subquery(&engine, parent_ids).await?;
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);
    assert!(!profiles_by_user.contains_key(&grace.id));

    // belongs_to: Order::load_user_via_subquery.
    let orders_table = Order::table();
    let foreign_key_ids = Select::from(&orders_table).columns([orders_table.col("user_id")]);
    let users_by_id: HashMap<i64, User> =
        Order::load_user_via_subquery(&engine, foreign_key_ids).await?;
    assert_eq!(users_by_id.get(&ada.id).unwrap(), &ada);
    assert_eq!(users_by_id.get(&grace.id).unwrap(), &grace);

    // many_to_many: Post::load_tags_via_subquery.
    let posts_table = Post::table();
    let parent_ids = Select::from(&posts_table).columns([posts_table.col("id")]);
    let tags_by_post: HashMap<i64, Vec<Tag>> =
        Post::load_tags_via_subquery(&engine, parent_ids).await?;
    assert_eq!(
        tags_by_post.get(&rust_post.id).unwrap(),
        &vec![rust_tag.clone()]
    );

    Ok(())
}

#[tokio::test]
async fn derive_generated_joined_methods_work_end_to_end() -> rusty_db::Result<()> {
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

    let ada_order = Order {
        id: 1,
        user_id: 1,
        amount: 100,
    };
    let grace_order = Order {
        id: 2,
        user_id: 2,
        amount: 200,
    };
    engine.execute(&ada_order.insert()).await?;
    engine.execute(&grace_order.insert()).await?;

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;

    // has_many: User::load_orders_joined.
    let (users, orders_by_user): (Vec<User>, HashMap<i64, Vec<Order>>) =
        User::load_orders_joined(&engine, None).await?;
    let mut user_ids: Vec<i64> = users.iter().map(|u| u.id).collect();
    user_ids.sort();
    assert_eq!(user_ids, vec![1, 2]);
    assert_eq!(
        orders_by_user.get(&ada.id).unwrap(),
        &vec![ada_order.clone()]
    );
    assert_eq!(
        orders_by_user.get(&grace.id).unwrap(),
        &vec![grace_order.clone()]
    );

    // has_one: User::load_profile_joined, with a filter this time.
    let filter = User::table().col("id").eq(ada.id);
    let (filtered_users, profiles_by_user): (Vec<User>, HashMap<i64, Profile>) =
        User::load_profile_joined(&engine, Some(filter)).await?;
    assert_eq!(filtered_users, vec![ada.clone()]);
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);

    // belongs_to: Order::load_user_joined.
    let (orders, users_by_id): (Vec<Order>, HashMap<i64, User>) =
        Order::load_user_joined(&engine, None).await?;
    let mut order_ids: Vec<i64> = orders.iter().map(|o| o.id).collect();
    order_ids.sort();
    assert_eq!(order_ids, vec![1, 2]);
    assert_eq!(users_by_id.get(&ada.id).unwrap(), &ada);
    assert_eq!(users_by_id.get(&grace.id).unwrap(), &grace);

    // many_to_many: Post::load_tags_joined.
    let (posts, tags_by_post): (Vec<Post>, HashMap<i64, Vec<Tag>>) =
        Post::load_tags_joined(&engine, None).await?;
    assert_eq!(posts, vec![rust_post.clone()]);
    assert_eq!(tags_by_post.get(&rust_post.id).unwrap(), &vec![rust_tag]);

    Ok(())
}

#[tokio::test]
async fn has_many_joined_from_query_accepts_an_arbitrary_caller_built_select(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // A third user with no orders at all — excluded by the join below,
    // same as it would be from a plain `filter`.
    let linus = User {
        id: 3,
        name: "linus".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;
    engine.execute(&linus.insert()).await?;

    let ada_small_order = Order {
        id: 1,
        user_id: 1,
        amount: 100,
    };
    let ada_tiny_order = Order {
        id: 2,
        user_id: 1,
        amount: 50,
    };
    let grace_big_order = Order {
        id: 3,
        user_id: 2,
        amount: 200,
    };
    engine.execute(&ada_small_order.insert()).await?;
    engine.execute(&ada_tiny_order.insert()).await?;
    engine.execute(&grace_big_order.insert()).await?;

    // Unlike `load_many_joined`'s plain `filter`, this Select brings its
    // own JOIN — a real capability gap `filter` alone can't cover: "every
    // user with an order over 100" (only grace's order qualifies).
    let users_table = User::table();
    let orders_table = Order::table();
    let parents = Select::from(&users_table)
        .join(
            &orders_table,
            users_table.col("id").eq_col(&orders_table.col("user_id")),
        )
        .columns(User::COLUMNS.iter().map(|c| users_table.col(*c)))
        .filter(orders_table.col("amount").gt(100_i64))
        .distinct();

    let (parents, orders_by_user): (Vec<User>, HashMap<i64, Vec<Order>>) =
        rusty_db::relations::load_many_joined_from_query(&engine, parents, "id", "user_id").await?;

    assert_eq!(parents, vec![grace.clone()]);
    assert_eq!(
        orders_by_user.get(&grace.id).unwrap(),
        &vec![grace_big_order]
    );
    assert!(!orders_by_user.contains_key(&ada.id));
    assert!(!orders_by_user.contains_key(&linus.id));

    Ok(())
}

#[tokio::test]
async fn derive_generated_joined_from_query_methods_work_end_to_end() -> rusty_db::Result<()> {
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

    let ada_order = Order {
        id: 1,
        user_id: 1,
        amount: 100,
    };
    let grace_order = Order {
        id: 2,
        user_id: 2,
        amount: 200,
    };
    engine.execute(&ada_order.insert()).await?;
    engine.execute(&grace_order.insert()).await?;

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;

    let users_table = User::table();
    let full_users =
        || Select::from(&users_table).columns(User::COLUMNS.iter().map(|c| users_table.col(*c)));

    // has_many: User::load_orders_joined_from_query.
    let (users, orders_by_user): (Vec<User>, HashMap<i64, Vec<Order>>) =
        User::load_orders_joined_from_query(&engine, full_users()).await?;
    let mut user_ids: Vec<i64> = users.iter().map(|u| u.id).collect();
    user_ids.sort();
    assert_eq!(user_ids, vec![1, 2]);
    assert_eq!(
        orders_by_user.get(&ada.id).unwrap(),
        &vec![ada_order.clone()]
    );
    assert_eq!(
        orders_by_user.get(&grace.id).unwrap(),
        &vec![grace_order.clone()]
    );

    // has_one: User::load_profile_joined_from_query, narrowed by the
    // caller's own filter this time (proving the widened contract, not
    // just a like-for-like replay of the plain-filter test above).
    let ada_only = full_users().filter(users_table.col("id").eq(ada.id));
    let (filtered_users, profiles_by_user): (Vec<User>, HashMap<i64, Profile>) =
        User::load_profile_joined_from_query(&engine, ada_only).await?;
    assert_eq!(filtered_users, vec![ada.clone()]);
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);

    // belongs_to: Order::load_user_joined_from_query.
    let orders_table = Order::table();
    let full_orders =
        Select::from(&orders_table).columns(Order::COLUMNS.iter().map(|c| orders_table.col(*c)));
    let (orders, users_by_id): (Vec<Order>, HashMap<i64, User>) =
        Order::load_user_joined_from_query(&engine, full_orders).await?;
    let mut order_ids: Vec<i64> = orders.iter().map(|o| o.id).collect();
    order_ids.sort();
    assert_eq!(order_ids, vec![1, 2]);
    assert_eq!(users_by_id.get(&ada.id).unwrap(), &ada);
    assert_eq!(users_by_id.get(&grace.id).unwrap(), &grace);

    // many_to_many: Post::load_tags_joined_from_query.
    let posts_table = Post::table();
    let full_posts =
        Select::from(&posts_table).columns(Post::COLUMNS.iter().map(|c| posts_table.col(*c)));
    let (posts, tags_by_post): (Vec<Post>, HashMap<i64, Vec<Tag>>) =
        Post::load_tags_joined_from_query(&engine, full_posts).await?;
    assert_eq!(posts, vec![rust_post.clone()]);
    assert_eq!(tags_by_post.get(&rust_post.id).unwrap(), &vec![rust_tag]);

    Ok(())
}

#[tokio::test]
async fn has_many_joined_matches_the_select_in_result_despite_colliding_id_columns(
) -> rusty_db::Result<()> {
    // User and Order both map their own "id" column — proving the joined
    // strategy's internal per-side column aliasing actually works, not
    // just that it happens not to matter for this particular schema.
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // A third user with no orders at all.
    let linus = User {
        id: 3,
        name: "linus".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;
    engine.execute(&linus.insert()).await?;

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

    let (parents, orders_by_user): (Vec<User>, HashMap<i64, Vec<Order>>) =
        rusty_db::relations::load_many_joined(&engine, None, "id", "user_id").await?;

    // Every user comes back exactly once, including the childless one —
    // the join's per-child row repetition was correctly deduplicated.
    let mut parent_ids: Vec<i64> = parents.iter().map(|u| u.id).collect();
    parent_ids.sort();
    assert_eq!(parent_ids, vec![1, 2, 3]);

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
    // A childless parent still appears in the parent list, but has no
    // entry at all in the children map — same as `load_many`'s shape.
    assert!(!orders_by_user.contains_key(&linus.id));

    Ok(())
}

#[tokio::test]
async fn has_many_joined_with_a_filter_narrows_the_parent_batch() -> rusty_db::Result<()> {
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
                user_id: 2,
                amount: 200,
            })
            .insert(),
        )
        .await?;

    let filter = User::table().col("id").eq(1_i64);
    let (parents, orders_by_user): (Vec<User>, HashMap<i64, Vec<Order>>) =
        rusty_db::relations::load_many_joined(&engine, Some(filter), "id", "user_id").await?;

    assert_eq!(parents, vec![ada.clone()]);
    assert!(orders_by_user.contains_key(&ada.id));
    // grace's order genuinely exists but grace was excluded by the filter.
    assert!(!orders_by_user.contains_key(&grace.id));

    Ok(())
}

#[tokio::test]
async fn has_one_joined_matches_the_select_in_result_despite_colliding_id_columns(
) -> rusty_db::Result<()> {
    // User and Profile both map their own "id" column too.
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
    };
    // A third user with no profile at all.
    let linus = User {
        id: 3,
        name: "linus".to_string(),
    };
    engine.execute(&ada.insert()).await?;
    engine.execute(&grace.insert()).await?;
    engine.execute(&linus.insert()).await?;

    let ada_profile = Profile {
        id: 1,
        user_id: 1,
        bio: "mathematician".to_string(),
    };
    engine.execute(&ada_profile.insert()).await?;

    let (parents, profiles_by_user): (Vec<User>, HashMap<i64, Profile>) =
        rusty_db::relations::load_has_one_joined(&engine, None, "id", "user_id").await?;

    let mut parent_ids: Vec<i64> = parents.iter().map(|u| u.id).collect();
    parent_ids.sort();
    assert_eq!(parent_ids, vec![1, 2, 3]);

    assert_eq!(profiles_by_user.len(), 1); // grace and linus have no entry at all
    assert_eq!(profiles_by_user.get(&ada.id).unwrap(), &ada_profile);
    assert!(!profiles_by_user.contains_key(&grace.id));
    assert!(!profiles_by_user.contains_key(&linus.id));

    Ok(())
}

#[tokio::test]
async fn has_one_joined_reports_a_conflict_when_a_parent_has_more_than_one_matching_row(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    engine.execute(&ada.insert()).await?;

    // Two profiles for the same user: not actually a one-to-one relationship.
    engine
        .execute(
            &(Profile {
                id: 1,
                user_id: 1,
                bio: "mathematician".to_string(),
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Profile {
                id: 2,
                user_id: 1,
                bio: "also a mathematician".to_string(),
            })
            .insert(),
        )
        .await?;

    let result: rusty_db::Result<(Vec<User>, HashMap<i64, Profile>)> =
        rusty_db::relations::load_has_one_joined(&engine, None, "id", "user_id").await;
    assert!(matches!(result, Err(rusty_db::Error::Conflict(_))));

    Ok(())
}

#[tokio::test]
async fn belongs_to_joined_matches_the_select_in_result_despite_colliding_id_columns(
) -> rusty_db::Result<()> {
    // Order and User both map their own "id" column too.
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

    let (children, users_by_id): (Vec<Order>, HashMap<i64, User>) =
        rusty_db::relations::load_one_joined(&engine, None, "user_id", "id").await?;

    // Every order comes back exactly once — no dedup needed on this side.
    let mut child_ids: Vec<i64> = children.iter().map(|o| o.id).collect();
    child_ids.sort();
    assert_eq!(child_ids, vec![1, 2, 3]);

    // Two orders share user_id 1 -> the map still has exactly one entry for it.
    assert_eq!(users_by_id.len(), 2);
    assert_eq!(users_by_id.get(&ada.id).unwrap(), &ada);
    assert_eq!(users_by_id.get(&grace.id).unwrap(), &grace);

    Ok(())
}

#[tokio::test]
async fn belongs_to_joined_with_a_filter_narrows_the_child_batch() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    engine.execute(&ada.insert()).await?;

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
                amount: 999,
            })
            .insert(),
        )
        .await?;

    let filter = Order::table().col("amount").eq(100_i64);
    let (children, users_by_id): (Vec<Order>, HashMap<i64, User>) =
        rusty_db::relations::load_one_joined(&engine, Some(filter), "user_id", "id").await?;

    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, 1);
    assert_eq!(users_by_id.get(&ada.id).unwrap(), &ada);

    Ok(())
}

#[tokio::test]
async fn many_to_many_joined_matches_the_select_in_result_despite_colliding_id_columns(
) -> rusty_db::Result<()> {
    // Post and Tag both map their own "id" column too.
    let engine = engine_with_schema().await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    let db_post = Post {
        id: 2,
        title: "Databases 101".to_string(),
    };
    // A third post with no tags at all, to prove it still appears in the
    // parent list but has no entry in the targets map.
    let untagged_post = Post {
        id: 3,
        title: "Untagged".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    engine.execute(&db_post.insert()).await?;
    engine.execute(&untagged_post.insert()).await?;

    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    let db_tag = Tag {
        id: 2,
        name: "databases".to_string(),
    };
    let systems_tag = Tag {
        id: 3,
        name: "systems".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    engine.execute(&db_tag.insert()).await?;
    engine.execute(&systems_tag.insert()).await?;

    // rust_post: rust + systems; db_post: rust + databases.
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, rust_post.id, systems_tag.id).await?;
    insert_post_tag(&engine, db_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, db_post.id, db_tag.id).await?;

    let (parents, tags_by_post): (Vec<Post>, HashMap<i64, Vec<Tag>>) =
        rusty_db::relations::load_many_to_many_joined(
            &engine,
            None,
            "id",
            "post_tags",
            "post_id",
            "tag_id",
            "id",
        )
        .await?;

    // Every post comes back exactly once, including the untagged one — the
    // join's per-tag row repetition was correctly deduplicated.
    let mut parent_ids: Vec<i64> = parents.iter().map(|p| p.id).collect();
    parent_ids.sort();
    assert_eq!(parent_ids, vec![1, 2, 3]);

    let mut rust_post_tags = tags_by_post.get(&rust_post.id).unwrap().clone();
    rust_post_tags.sort_by_key(|t| t.id);
    assert_eq!(rust_post_tags, vec![rust_tag.clone(), systems_tag.clone()]);
    let mut db_post_tags = tags_by_post.get(&db_post.id).unwrap().clone();
    db_post_tags.sort_by_key(|t| t.id);
    assert_eq!(db_post_tags, vec![rust_tag.clone(), db_tag.clone()]);
    // A targetless parent still appears in the parent list, but has no
    // entry at all in the targets map — same as `load_many_to_many`'s shape.
    assert!(!tags_by_post.contains_key(&untagged_post.id));

    Ok(())
}

#[tokio::test]
async fn many_to_many_joined_with_a_filter_narrows_the_parent_batch() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let rust_post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    let db_post = Post {
        id: 2,
        title: "Databases 101".to_string(),
    };
    engine.execute(&rust_post.insert()).await?;
    engine.execute(&db_post.insert()).await?;

    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;
    insert_post_tag(&engine, rust_post.id, rust_tag.id).await?;
    insert_post_tag(&engine, db_post.id, rust_tag.id).await?;

    let filter = Post::table().col("id").eq(1_i64);
    let (parents, tags_by_post): (Vec<Post>, HashMap<i64, Vec<Tag>>) =
        rusty_db::relations::load_many_to_many_joined(
            &engine,
            Some(filter),
            "id",
            "post_tags",
            "post_id",
            "tag_id",
            "id",
        )
        .await?;

    assert_eq!(parents, vec![rust_post.clone()]);
    assert!(tags_by_post.contains_key(&rust_post.id));
    // db_post's tag genuinely exists but db_post was excluded by the filter.
    assert!(!tags_by_post.contains_key(&db_post.id));

    Ok(())
}

#[tokio::test]
async fn delete_cascading_deletes_cascade_delete_children_and_the_parent() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let ada = User {
        id: 1,
        name: "ada".to_string(),
    };
    engine.execute(&ada.insert()).await?;
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
            &(Profile {
                id: 1,
                user_id: 1,
                bio: "mathematician".to_string(),
            })
            .insert(),
        )
        .await?;

    ada.delete_cascading(&engine).await?;

    let users: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(users.is_empty(), "the user itself should be deleted");
    let orders: Vec<Order> = engine.fetch_all_as(&Select::from(&Order::table())).await?;
    assert!(
        orders.is_empty(),
        "cascade = \"delete\" has_many children should be deleted too"
    );
    let profiles: Vec<Profile> = engine
        .fetch_all_as(&Select::from(&Profile::table()))
        .await?;
    assert!(
        profiles.is_empty(),
        "cascade = \"delete\" has_one child should be deleted too"
    );

    Ok(())
}

#[tokio::test]
async fn delete_cascading_does_not_touch_a_different_users_children() -> rusty_db::Result<()> {
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
    engine
        .execute(
            &(Order {
                id: 1,
                user_id: 2,
                amount: 200,
            })
            .insert(),
        )
        .await?;

    ada.delete_cascading(&engine).await?;

    let users: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(users, vec![grace]);
    let orders: Vec<Order> = engine.fetch_all_as(&Select::from(&Order::table())).await?;
    assert_eq!(orders.len(), 1, "grace's order should be untouched");

    Ok(())
}

#[tokio::test]
async fn delete_cascading_orphans_instead_of_deleting_in_orphan_mode() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let team = Team {
        id: 1,
        name: "rustaceans".to_string(),
    };
    engine.execute(&team.insert()).await?;
    engine
        .execute(
            &(Player {
                id: 1,
                team_id: Some(1),
                name: "ferris".to_string(),
            })
            .insert(),
        )
        .await?;
    engine
        .execute(
            &(Player {
                id: 2,
                team_id: Some(1),
                name: "cargo".to_string(),
            })
            .insert(),
        )
        .await?;

    team.delete_cascading(&engine).await?;

    let teams: Vec<Team> = engine.fetch_all_as(&Select::from(&Team::table())).await?;
    assert!(teams.is_empty(), "the team itself should be deleted");

    let mut players: Vec<Player> = engine.fetch_all_as(&Select::from(&Player::table())).await?;
    players.sort_by_key(|p| p.id);
    assert_eq!(
        players,
        vec![
            Player {
                id: 1,
                team_id: None,
                name: "ferris".to_string(),
            },
            Player {
                id: 2,
                team_id: None,
                name: "cargo".to_string(),
            },
        ],
        "cascade = \"orphan\" players should survive, with their foreign key nulled out"
    );

    Ok(())
}

#[tokio::test]
async fn delete_cascading_many_to_many_deletes_join_rows_but_not_the_targets(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let post = Post {
        id: 1,
        title: "Why Rust".to_string(),
    };
    let other_post = Post {
        id: 2,
        title: "Also About Rust".to_string(),
    };
    engine.execute(&post.insert()).await?;
    engine.execute(&other_post.insert()).await?;

    let rust_tag = Tag {
        id: 1,
        name: "rust".to_string(),
    };
    engine.execute(&rust_tag.insert()).await?;

    // Both posts share the same tag, so deleting one post's join rows must
    // not touch the tag itself (still referenced by the other post) or the
    // other post's own join row.
    insert_post_tag(&engine, post.id, rust_tag.id).await?;
    insert_post_tag(&engine, other_post.id, rust_tag.id).await?;

    post.delete_cascading(&engine).await?;

    let posts: Vec<Post> = engine.fetch_all_as(&Select::from(&Post::table())).await?;
    assert_eq!(posts, vec![other_post.clone()]);

    let tags: Vec<Tag> = engine.fetch_all_as(&Select::from(&Tag::table())).await?;
    assert_eq!(
        tags,
        vec![rust_tag.clone()],
        "the shared tag must survive — many_to_many cascade only deletes join rows"
    );

    let remaining_tags_by_post: HashMap<i64, Vec<Tag>> =
        Post::load_tags(&engine, std::slice::from_ref(&other_post)).await?;
    assert_eq!(
        remaining_tags_by_post.get(&other_post.id).unwrap(),
        &vec![rust_tag]
    );

    Ok(())
}
