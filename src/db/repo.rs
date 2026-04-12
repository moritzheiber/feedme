use sqlx::{Row, SqlitePool};

use super::models::{Favicon, Feed, Item};

pub async fn insert_feed(
    pool: &SqlitePool,
    url: &str,
    fetch_interval_minutes: i64,
) -> Result<Feed, sqlx::Error> {
    sqlx::query_as::<_, Feed>(
        "INSERT INTO feeds (url, fetch_interval_minutes) VALUES (?, ?) RETURNING *",
    )
    .bind(url)
    .bind(fetch_interval_minutes)
    .fetch_one(pool)
    .await
}

pub async fn list_feeds(pool: &SqlitePool) -> Result<Vec<Feed>, sqlx::Error> {
    sqlx::query_as::<_, Feed>("SELECT * FROM feeds ORDER BY id")
        .fetch_all(pool)
        .await
}

pub async fn update_feed(
    pool: &SqlitePool,
    id: i64,
    url: Option<&str>,
    interval: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE feeds SET url = COALESCE(?, url), fetch_interval_minutes = COALESCE(?, fetch_interval_minutes) WHERE id = ?",
    )
    .bind(url)
    .bind(interval)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_feed(pool: &SqlitePool, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM feeds WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn insert_item(
    pool: &SqlitePool,
    feed_id: i64,
    title: &str,
    author: &str,
    html: &str,
    url: &str,
    created_on_time: i64,
) -> Result<Item, sqlx::Error> {
    sqlx::query_as::<_, Item>(
        "INSERT INTO items (feed_id, title, author, html, url, created_on_time) VALUES (?, ?, ?, ?, ?, ?) RETURNING *",
    )
    .bind(feed_id)
    .bind(title)
    .bind(author)
    .bind(html)
    .bind(url)
    .bind(created_on_time)
    .fetch_one(pool)
    .await
}

pub async fn get_items_since(
    pool: &SqlitePool,
    since_id: i64,
    limit: i64,
) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>("SELECT * FROM items WHERE id > ? ORDER BY id ASC LIMIT ?")
        .bind(since_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

pub async fn get_items_before(
    pool: &SqlitePool,
    max_id: i64,
    limit: i64,
) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query_as::<_, Item>("SELECT * FROM items WHERE id < ? ORDER BY id DESC LIMIT ?")
        .bind(max_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

pub async fn get_items_by_ids(pool: &SqlitePool, ids: &[i64]) -> Result<Vec<Item>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders: String = itertools_join(ids.len());
    let query = format!("SELECT * FROM items WHERE id IN ({placeholders}) LIMIT 50");
    let mut q = sqlx::query_as::<_, Item>(&query);
    for id in ids {
        q = q.bind(id);
    }
    q.fetch_all(pool).await
}

pub async fn get_total_items(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT COUNT(*) as count FROM items")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("count"))
}

pub async fn get_unread_item_ids(pool: &SqlitePool) -> Result<Vec<i64>, sqlx::Error> {
    let rows = sqlx::query("SELECT id FROM items WHERE is_read = 0 ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| r.get::<i64, _>("id")).collect())
}

pub async fn get_saved_item_ids(pool: &SqlitePool) -> Result<Vec<i64>, sqlx::Error> {
    let rows = sqlx::query("SELECT id FROM items WHERE is_saved = 1 ORDER BY id")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(|r| r.get::<i64, _>("id")).collect())
}

pub async fn mark_item(
    pool: &SqlitePool,
    id: i64,
    field: &str,
    value: i64,
) -> Result<bool, sqlx::Error> {
    let query = match field {
        "is_read" => "UPDATE items SET is_read = ? WHERE id = ?",
        "is_saved" => "UPDATE items SET is_saved = ? WHERE id = ?",
        _ => return Ok(false),
    };
    let result = sqlx::query(query)
        .bind(value)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn mark_feed_read(
    pool: &SqlitePool,
    feed_id: i64,
    before: i64,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("UPDATE items SET is_read = 1 WHERE feed_id = ? AND created_on_time <= ?")
            .bind(feed_id)
            .bind(before)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

pub async fn mark_all_read(pool: &SqlitePool, before: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("UPDATE items SET is_read = 1 WHERE created_on_time <= ?")
        .bind(before)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn unread_recently_read(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("UPDATE items SET is_read = 0 WHERE is_read = 1")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn get_favicons(pool: &SqlitePool) -> Result<Vec<Favicon>, sqlx::Error> {
    sqlx::query_as::<_, Favicon>("SELECT * FROM favicons ORDER BY id")
        .fetch_all(pool)
        .await
}

pub async fn last_refreshed_on_time(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT COALESCE(MAX(last_updated_on_time), 0) as t FROM feeds")
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("t"))
}

pub async fn item_exists_by_url(
    pool: &SqlitePool,
    feed_id: i64,
    url: &str,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query("SELECT COUNT(*) as count FROM items WHERE feed_id = ? AND url = ?")
        .bind(feed_id)
        .bind(url)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("count") > 0)
}

pub async fn update_feed_title_and_site(
    pool: &SqlitePool,
    id: i64,
    title: &str,
    site_url: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE feeds SET title = ?, site_url = ? WHERE id = ?")
        .bind(title)
        .bind(site_url)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_feed_favicon(
    pool: &SqlitePool,
    feed_id: i64,
    favicon_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE feeds SET favicon_id = ? WHERE id = ?")
        .bind(favicon_id)
        .bind(feed_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_feed_last_updated(
    pool: &SqlitePool,
    id: i64,
    timestamp: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE feeds SET last_updated_on_time = ? WHERE id = ?")
        .bind(timestamp)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_feed_schedule(
    pool: &SqlitePool,
    id: i64,
    feed_ttl_minutes: i64,
    skip_hours_mask: i64,
    skip_days_mask: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE feeds SET feed_ttl_minutes = ?, skip_hours_mask = ?, skip_days_mask = ? WHERE id = ?",
    )
    .bind(feed_ttl_minutes)
    .bind(skip_hours_mask)
    .bind(skip_days_mask)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn feed_exists_by_url(pool: &SqlitePool, url: &str) -> Result<bool, sqlx::Error> {
    let row = sqlx::query("SELECT COUNT(*) as count FROM feeds WHERE url = ?")
        .bind(url)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("count") > 0)
}

fn itertools_join(count: usize) -> String {
    vec!["?"; count].join(", ")
}

pub async fn increment_failures(pool: &SqlitePool, feed_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE feeds SET consecutive_failures = consecutive_failures + 1 WHERE id = ?",
    )
    .bind(feed_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn reset_failures(pool: &SqlitePool, feed_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE feeds SET consecutive_failures = 0 WHERE id = ?")
        .bind(feed_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn upsert_favicon_with_etag(
    pool: &SqlitePool,
    data: &str,
    etag: &str,
) -> Result<Favicon, sqlx::Error> {
    sqlx::query_as::<_, Favicon>("INSERT INTO favicons (data, etag) VALUES (?, ?) RETURNING *")
        .bind(data)
        .bind(etag)
        .fetch_one(pool)
        .await
}

pub async fn update_favicon_data_and_etag(
    pool: &SqlitePool,
    favicon_id: i64,
    data: &str,
    etag: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE favicons SET data = ?, etag = ? WHERE id = ?")
        .bind(data)
        .bind(etag)
        .bind(favicon_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_favicon(pool: &SqlitePool, id: i64) -> Result<Option<Favicon>, sqlx::Error> {
    sqlx::query_as::<_, Favicon>("SELECT * FROM favicons WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn update_favicon_last_checked(
    pool: &SqlitePool,
    feed_id: i64,
    timestamp: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE feeds SET favicon_last_checked = ? WHERE id = ?")
        .bind(timestamp)
        .bind(feed_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
pub async fn get_feed(pool: &SqlitePool, id: i64) -> Result<Option<Feed>, sqlx::Error> {
    sqlx::query_as::<_, Feed>("SELECT * FROM feeds WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

#[cfg(test)]
pub async fn upsert_favicon(pool: &SqlitePool, data: &str) -> Result<Favicon, sqlx::Error> {
    sqlx::query_as::<_, Favicon>("INSERT INTO favicons (data) VALUES (?) RETURNING *")
        .bind(data)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn insert_and_list_feeds() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        assert_eq!(feed.url, "https://example.com/feed.xml");
        assert_eq!(feed.fetch_interval_minutes, 60);
        assert!(feed.id > 0);

        let feeds = list_feeds(&pool).await.unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].url, "https://example.com/feed.xml");
    }

    #[tokio::test]
    async fn get_feed_by_id() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        let found = get_feed(&pool, feed.id).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().url, "https://example.com/feed.xml");

        let not_found = get_feed(&pool, 9999).await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn update_feed_url_and_interval() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let updated = update_feed(
            &pool,
            feed.id,
            Some("https://example.com/new.xml"),
            Some(120),
        )
        .await
        .unwrap();
        assert!(updated);

        let found = get_feed(&pool, feed.id).await.unwrap().unwrap();
        assert_eq!(found.url, "https://example.com/new.xml");
        assert_eq!(found.fetch_interval_minutes, 120);
    }

    #[tokio::test]
    async fn update_feed_partial() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        update_feed(&pool, feed.id, None, Some(30)).await.unwrap();
        let found = get_feed(&pool, feed.id).await.unwrap().unwrap();
        assert_eq!(found.url, "https://example.com/feed.xml");
        assert_eq!(found.fetch_interval_minutes, 30);
    }

    #[tokio::test]
    async fn delete_feed_cascades_items() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        insert_item(
            &pool,
            feed.id,
            "Title",
            "Author",
            "<p>Hi</p>",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();

        let deleted = delete_feed(&pool, feed.id).await.unwrap();
        assert!(deleted);

        let feeds = list_feeds(&pool).await.unwrap();
        assert!(feeds.is_empty());

        let total = get_total_items(&pool).await.unwrap();
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn delete_nonexistent_feed() {
        let pool = test_pool().await;
        let deleted = delete_feed(&pool, 9999).await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn insert_and_query_items() {
        let pool = test_pool().await;

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        for i in 1..=5 {
            insert_item(
                &pool,
                feed.id,
                &format!("Title {i}"),
                "Author",
                "<p>Hi</p>",
                &format!("https://example.com/{i}"),
                1700000000 + i,
            )
            .await
            .unwrap();
        }

        let total = get_total_items(&pool).await.unwrap();
        assert_eq!(total, 5);
    }

    #[tokio::test]
    async fn get_items_since_id() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let mut ids = vec![];
        for i in 1..=5 {
            let item = insert_item(
                &pool,
                feed.id,
                &format!("Title {i}"),
                "",
                "",
                &format!("https://example.com/{i}"),
                1700000000 + i,
            )
            .await
            .unwrap();
            ids.push(item.id);
        }

        let items = get_items_since(&pool, ids[1], 50).await.unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.id > ids[1]));
    }

    #[tokio::test]
    async fn get_items_before_max_id() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let mut ids = vec![];
        for i in 1..=5 {
            let item = insert_item(
                &pool,
                feed.id,
                &format!("Title {i}"),
                "",
                "",
                &format!("https://example.com/{i}"),
                1700000000 + i,
            )
            .await
            .unwrap();
            ids.push(item.id);
        }

        let items = get_items_before(&pool, ids[3], 50).await.unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.id < ids[3]));
    }

    #[tokio::test]
    async fn get_items_by_specific_ids() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let mut ids = vec![];
        for i in 1..=5 {
            let item = insert_item(
                &pool,
                feed.id,
                &format!("Title {i}"),
                "",
                "",
                &format!("https://example.com/{i}"),
                1700000000 + i,
            )
            .await
            .unwrap();
            ids.push(item.id);
        }

        let requested = &[ids[0], ids[2], ids[4]];
        let items = get_items_by_ids(&pool, requested).await.unwrap();
        assert_eq!(items.len(), 3);
    }

    #[tokio::test]
    async fn items_pagination_limit() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        for i in 1..=100 {
            insert_item(
                &pool,
                feed.id,
                &format!("Title {i}"),
                "",
                "",
                &format!("https://example.com/{i}"),
                1700000000 + i,
            )
            .await
            .unwrap();
        }

        let items = get_items_since(&pool, 0, 50).await.unwrap();
        assert_eq!(items.len(), 50);
    }

    #[tokio::test]
    async fn unread_and_saved_item_ids() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let item1 = insert_item(
            &pool,
            feed.id,
            "A",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();
        let item2 = insert_item(
            &pool,
            feed.id,
            "B",
            "",
            "",
            "https://example.com/2",
            1700000001,
        )
        .await
        .unwrap();

        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert_eq!(unread.len(), 2);

        mark_item(&pool, item1.id, "is_read", 1).await.unwrap();
        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0], item2.id);

        mark_item(&pool, item2.id, "is_saved", 1).await.unwrap();
        let saved = get_saved_item_ids(&pool).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0], item2.id);
    }

    #[tokio::test]
    async fn mark_feed_read_before_timestamp() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        insert_item(&pool, feed.id, "Old", "", "", "https://example.com/1", 1000)
            .await
            .unwrap();
        insert_item(&pool, feed.id, "New", "", "", "https://example.com/2", 2000)
            .await
            .unwrap();

        let affected = mark_feed_read(&pool, feed.id, 1500).await.unwrap();
        assert_eq!(affected, 1);

        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert_eq!(unread.len(), 1);
    }

    #[tokio::test]
    async fn mark_all_read_before_timestamp() {
        let pool = test_pool().await;
        let feed1 = insert_feed(&pool, "https://example.com/feed1.xml", 60)
            .await
            .unwrap();
        let feed2 = insert_feed(&pool, "https://example.com/feed2.xml", 60)
            .await
            .unwrap();

        insert_item(
            &pool,
            feed1.id,
            "Old",
            "",
            "",
            "https://example.com/1",
            1000,
        )
        .await
        .unwrap();
        insert_item(
            &pool,
            feed2.id,
            "Also old",
            "",
            "",
            "https://example.com/2",
            1200,
        )
        .await
        .unwrap();
        insert_item(
            &pool,
            feed1.id,
            "New",
            "",
            "",
            "https://example.com/3",
            3000,
        )
        .await
        .unwrap();

        let affected = mark_all_read(&pool, 1500).await.unwrap();
        assert_eq!(affected, 2);

        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert_eq!(unread.len(), 1);
    }

    #[tokio::test]
    async fn unread_recently_read_items() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let item = insert_item(
            &pool,
            feed.id,
            "A",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();
        mark_item(&pool, item.id, "is_read", 1).await.unwrap();

        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert!(unread.is_empty());

        let affected = unread_recently_read(&pool).await.unwrap();
        assert!(affected > 0);

        let unread = get_unread_item_ids(&pool).await.unwrap();
        assert_eq!(unread.len(), 1);
    }

    #[tokio::test]
    async fn upsert_and_get_favicons() {
        let pool = test_pool().await;

        let fav = upsert_favicon(&pool, "image/gif;base64,AAAA")
            .await
            .unwrap();
        assert!(fav.id > 0);

        let favicons = get_favicons(&pool).await.unwrap();
        assert_eq!(favicons.len(), 1);
        assert_eq!(favicons[0].data, "image/gif;base64,AAAA");
    }

    #[tokio::test]
    async fn last_refreshed_time() {
        let pool = test_pool().await;

        let t = last_refreshed_on_time(&pool).await.unwrap();
        assert_eq!(t, 0);

        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        update_feed_last_updated(&pool, feed.id, 1700000000)
            .await
            .unwrap();

        let t = last_refreshed_on_time(&pool).await.unwrap();
        assert_eq!(t, 1700000000);
    }

    #[tokio::test]
    async fn item_dedup_by_url() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let exists = item_exists_by_url(&pool, feed.id, "https://example.com/1")
            .await
            .unwrap();
        assert!(!exists);

        insert_item(
            &pool,
            feed.id,
            "A",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();

        let exists = item_exists_by_url(&pool, feed.id, "https://example.com/1")
            .await
            .unwrap();
        assert!(exists);
    }

    #[tokio::test]
    async fn update_feed_metadata() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        update_feed_title_and_site(&pool, feed.id, "My Feed", "https://example.com")
            .await
            .unwrap();
        let found = get_feed(&pool, feed.id).await.unwrap().unwrap();
        assert_eq!(found.title, "My Feed");
        assert_eq!(found.site_url, "https://example.com");
    }

    #[tokio::test]
    async fn update_feed_favicon_id() {
        let pool = test_pool().await;
        let feed = insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        let fav = upsert_favicon(&pool, "image/png;base64,AAAA")
            .await
            .unwrap();

        update_feed_favicon(&pool, feed.id, fav.id).await.unwrap();
        let found = get_feed(&pool, feed.id).await.unwrap().unwrap();
        assert_eq!(found.favicon_id, fav.id);
    }

    #[tokio::test]
    async fn feed_exists_by_url_check() {
        let pool = test_pool().await;

        assert!(
            !feed_exists_by_url(&pool, "https://example.com/feed.xml")
                .await
                .unwrap()
        );

        insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        assert!(
            feed_exists_by_url(&pool, "https://example.com/feed.xml")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn duplicate_feed_url_rejected() {
        let pool = test_pool().await;

        insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();
        let result = insert_feed(&pool, "https://example.com/feed.xml", 60).await;
        assert!(result.is_err());
    }
}
