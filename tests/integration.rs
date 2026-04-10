use axum::body::Body;
use http::Request;
use serde_json::Value;
use sqlx::SqlitePool;
use tower::ServiceExt;

async fn setup() -> (axum::Router, SqlitePool) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));
    let state = feedme::api::AppState::new(pool.clone(), api_key);
    let app = feedme::api::router(state);
    (app, pool)
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_response(app: axum::Router, req: Request<Body>) -> Value {
    let resp = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn full_lifecycle_add_feed_process_items_read_and_mark() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();

    let rss = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Example Blog</title>
    <link>https://example.com</link>
    <item>
      <title>Post One</title>
      <link>https://example.com/post-1</link>
      <description>First post content</description>
    </item>
    <item>
      <title>Post Two</title>
      <link>https://example.com/post-2</link>
      <description>Second post content</description>
    </item>
  </channel>
</rss>"#;
    let (inserted, _) = feedme::fetcher::process_feed(&pool, 1, rss).await.unwrap();
    assert_eq!(inserted, 2);

    let body = format!("api_key={}", api_key);
    let json = json_response(app.clone(), post("/?api&feeds", &body)).await;
    assert_eq!(json["auth"], 1);
    let feeds = json["feeds"].as_array().unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0]["title"], "Example Blog");

    let json = json_response(app.clone(), post("/?api&items", &body)).await;
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(json["total_items"], 2);

    let json = json_response(app.clone(), post("/?api&unread_item_ids", &body)).await;
    let unread_str = json["unread_item_ids"].as_str().unwrap();
    let unread_ids: Vec<&str> = unread_str.split(',').filter(|s| !s.is_empty()).collect();
    assert_eq!(unread_ids.len(), 2);

    let item_id = items[0]["id"].as_i64().unwrap();
    let mark_body = format!("api_key={}&mark=item&as=read&id={}", api_key, item_id);
    let json = json_response(app.clone(), post("/?api", &mark_body)).await;
    assert_eq!(json["auth"], 1);

    let json = json_response(app.clone(), post("/?api&unread_item_ids", &body)).await;
    let unread_str = json["unread_item_ids"].as_str().unwrap();
    let unread_ids: Vec<&str> = unread_str.split(',').filter(|s| !s.is_empty()).collect();
    assert_eq!(unread_ids.len(), 1);
}

#[tokio::test]
async fn save_and_unsave_item() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    let rss = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Blog</title><link>https://example.com</link>
<item><title>Post</title><link>https://example.com/post-1</link><description>Content</description></item>
</channel></rss>"#;
    feedme::fetcher::process_feed(&pool, 1, rss).await.unwrap();

    let json = json_response(
        app.clone(),
        post("/?api&items", &format!("api_key={}", api_key)),
    )
    .await;
    let item_id = json["items"][0]["id"].as_i64().unwrap();

    let save_body = format!("api_key={}&mark=item&as=saved&id={}", api_key, item_id);
    json_response(app.clone(), post("/?api", &save_body)).await;

    let json = json_response(
        app.clone(),
        post("/?api&saved_item_ids", &format!("api_key={}", api_key)),
    )
    .await;
    let saved_str = json["saved_item_ids"].as_str().unwrap();
    assert!(saved_str.contains(&item_id.to_string()));

    let unsave_body = format!("api_key={}&mark=item&as=unsaved&id={}", api_key, item_id);
    json_response(app.clone(), post("/?api", &unsave_body)).await;

    let json = json_response(
        app.clone(),
        post("/?api&saved_item_ids", &format!("api_key={}", api_key)),
    )
    .await;
    let saved_str = json["saved_item_ids"].as_str().unwrap();
    assert!(saved_str.is_empty() || !saved_str.contains(&item_id.to_string()));
}

#[tokio::test]
async fn mark_feed_read_before_timestamp() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();

    feedme::db::repo::insert_item(&pool, 1, "Old", "", "", "https://example.com/old", 1000)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "New", "", "", "https://example.com/new", 5000)
        .await
        .unwrap();

    let mark_body = format!("api_key={}&mark=feed&as=read&id=1&before=3000", api_key);
    let json = json_response(app.clone(), post("/?api", &mark_body)).await;
    assert_eq!(json["auth"], 1);

    let json = json_response(
        app.clone(),
        post("/?api&unread_item_ids", &format!("api_key={}", api_key)),
    )
    .await;
    let unread_str = json["unread_item_ids"].as_str().unwrap();
    let unread_ids: Vec<&str> = unread_str.split(',').filter(|s| !s.is_empty()).collect();
    assert_eq!(unread_ids.len(), 1);
}

#[tokio::test]
async fn unread_recently_read_restores_items() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "Post", "", "", "https://example.com/post", 1000)
        .await
        .unwrap();

    let mark_body = format!("api_key={}&mark=item&as=read&id=1", api_key);
    json_response(app.clone(), post("/?api", &mark_body)).await;

    let json = json_response(
        app.clone(),
        post("/?api&unread_item_ids", &format!("api_key={}", api_key)),
    )
    .await;
    assert!(json["unread_item_ids"].as_str().unwrap().is_empty());

    let unread_body = format!("api_key={}&unread_recently_read=1", api_key);
    json_response(app.clone(), post("/?api", &unread_body)).await;

    let json = json_response(
        app.clone(),
        post("/?api&unread_item_ids", &format!("api_key={}", api_key)),
    )
    .await;
    let unread_str = json["unread_item_ids"].as_str().unwrap();
    assert!(!unread_str.is_empty());
}

#[tokio::test]
async fn multiple_feeds_with_items() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_feed(&pool, "https://b.com/feed", 30)
        .await
        .unwrap();

    let rss_a = br#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Blog A</title><link>https://a.com</link>
<item><title>A1</title><link>https://a.com/1</link><description>c</description></item>
</channel></rss>"#;
    let rss_b = br#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Blog B</title><link>https://b.com</link>
<item><title>B1</title><link>https://b.com/1</link><description>c</description></item>
<item><title>B2</title><link>https://b.com/2</link><description>c</description></item>
</channel></rss>"#;
    feedme::fetcher::process_feed(&pool, 1, rss_a)
        .await
        .unwrap();
    feedme::fetcher::process_feed(&pool, 2, rss_b)
        .await
        .unwrap();

    let body = format!("api_key={}", api_key);
    let json = json_response(app.clone(), post("/?api&feeds", &body)).await;
    assert_eq!(json["feeds"].as_array().unwrap().len(), 2);

    let json = json_response(app.clone(), post("/?api&items", &body)).await;
    assert_eq!(json["total_items"], 3);
}

#[tokio::test]
async fn combined_query_returns_all_requested_data() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "Post", "", "", "https://example.com/post", 1000)
        .await
        .unwrap();

    let body = format!("api_key={}", api_key);
    let json = json_response(
        app.clone(),
        post("/?api&feeds&items&unread_item_ids&saved_item_ids", &body),
    )
    .await;

    assert!(json["feeds"].is_array());
    assert!(json["items"].is_array());
    assert!(json["unread_item_ids"].is_string());
    assert!(json["saved_item_ids"].is_string());
    assert_eq!(json["auth"], 1);
}

#[tokio::test]
async fn unauthenticated_request_gets_no_data() {
    let (app, _pool) = setup().await;

    let json = json_response(app.clone(), post("/?api&feeds&items", "api_key=wrong_key")).await;
    assert_eq!(json["auth"], 0);
    assert!(json.get("feeds").is_none());
    assert!(json.get("items").is_none());
}

#[tokio::test]
async fn process_feed_deduplicates_on_second_run() {
    let (_app, pool) = setup().await;

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();

    let rss = br#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Blog</title><link>https://example.com</link>
<item><title>Post</title><link>https://example.com/post</link><description>c</description></item>
</channel></rss>"#;

    let (first, _) = feedme::fetcher::process_feed(&pool, 1, rss).await.unwrap();
    assert_eq!(first, 1);

    let (second, _) = feedme::fetcher::process_feed(&pool, 1, rss).await.unwrap();
    assert_eq!(second, 0);
}

#[tokio::test]
async fn mark_nonexistent_item_returns_clean_response() {
    let (app, _pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    let body = format!("api_key={}&mark=item&as=read&id=99999", api_key);
    let json = json_response(app.clone(), post("/?api", &body)).await;
    assert_eq!(json["auth"], 1);
    assert!(json.get("last_refreshed_on_time").is_some());
}

#[tokio::test]
async fn mark_nonexistent_feed_returns_clean_response() {
    let (app, _pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    let body = format!("api_key={}&mark=feed&as=read&id=99999&before=9999", api_key);
    let json = json_response(app.clone(), post("/?api", &body)).await;
    assert_eq!(json["auth"], 1);
    assert!(json.get("last_refreshed_on_time").is_some());
}

#[tokio::test]
async fn unknown_mark_action_is_ignored() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "Post", "", "", "https://example.com/p", 1000)
        .await
        .unwrap();

    let body = format!("api_key={}&mark=item&as=banana&id=1", api_key);
    let json = json_response(app.clone(), post("/?api&unread_item_ids", &body)).await;
    assert_eq!(json["auth"], 1);
    let unread = json["unread_item_ids"].as_str().unwrap();
    assert!(unread.contains("1"));
}

#[tokio::test]
async fn malicious_html_in_feed_is_served_verbatim() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();

    let rss = br#"<?xml version="1.0"?><rss version="2.0"><channel>
<title>Blog</title><link>https://example.com</link>
<item><title>XSS Post</title><link>https://example.com/xss</link>
<description>&lt;script&gt;alert(1)&lt;/script&gt;&lt;img src=x onerror=alert(1)&gt;</description></item>
</channel></rss>"#;
    feedme::fetcher::process_feed(&pool, 1, rss).await.unwrap();

    let body = format!("api_key={}", api_key);
    let json = json_response(app.clone(), post("/?api&items", &body)).await;
    let html = json["items"][0]["html"].as_str().unwrap();
    assert!(html.contains("<script>"));
    assert!(html.contains("onerror="));
}

#[tokio::test]
async fn with_ids_garbage_values_are_silently_dropped() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "Post", "", "", "https://example.com/p", 1000)
        .await
        .unwrap();

    let body = format!("api_key={}", api_key);
    let json = json_response(
        app.clone(),
        post("/?api&items&with_ids=abc,,,,-1,NaN", &body),
    )
    .await;
    assert_eq!(json["auth"], 1);
    let items = json["items"].as_array().unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn empty_database_returns_zero_items() {
    let (app, _pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    let body = format!("api_key={}", api_key);
    let json = json_response(
        app.clone(),
        post("/?api&items&feeds&unread_item_ids", &body),
    )
    .await;
    assert_eq!(json["total_items"], 0);
    assert!(json["items"].as_array().unwrap().is_empty());
    assert!(json["feeds"].as_array().unwrap().is_empty());
    assert!(json["unread_item_ids"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn sql_injection_in_form_data_is_harmless() {
    let (app, pool) = setup().await;
    let api_key = format!("{:x}", md5::compute("test@example.com:secret"));

    feedme::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
        .await
        .unwrap();
    feedme::db::repo::insert_item(&pool, 1, "Post", "", "", "https://example.com/p", 1000)
        .await
        .unwrap();

    let body = format!("api_key={}", api_key);
    let json = json_response(
        app.clone(),
        post(
            "/?api&items&with_ids=1%20OR%201%3D1%3B%20DROP%20TABLE%20items%3B--",
            &body,
        ),
    )
    .await;
    assert_eq!(json["auth"], 1);
    assert!(json["items"].as_array().unwrap().is_empty());

    let json = json_response(app.clone(), post("/?api&items", &body)).await;
    assert_eq!(json["items"].as_array().unwrap().len(), 1);
}
