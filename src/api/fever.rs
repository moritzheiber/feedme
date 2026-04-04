use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Json;

use super::AppState;

#[derive(serde::Deserialize, Default)]
pub struct FeverQuery {
    pub feeds: Option<String>,
    pub favicons: Option<String>,
    pub items: Option<String>,
    pub unread_item_ids: Option<String>,
    pub saved_item_ids: Option<String>,
    pub since_id: Option<i64>,
    pub max_id: Option<i64>,
    pub with_ids: Option<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct FeverForm {
    pub api_key: Option<String>,
    pub mark: Option<String>,
    #[serde(rename = "as")]
    pub as_action: Option<String>,
    pub id: Option<i64>,
    pub before: Option<i64>,
    pub unread_recently_read: Option<i64>,
}

pub async fn handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FeverQuery>,
    axum::Form(form): axum::Form<FeverForm>,
) -> Json<serde_json::Value> {
    let mut response = serde_json::json!({
        "api_version": 3,
    });

    let authed = super::auth::authenticate(&state, form.api_key.as_deref());
    response["auth"] = serde_json::json!(if authed { 1 } else { 0 });

    if !authed {
        return Json(response);
    }

    let pool = &state.pool;

    let last_refreshed = crate::db::repo::last_refreshed_on_time(pool)
        .await
        .unwrap_or(0);
    response["last_refreshed_on_time"] = serde_json::json!(last_refreshed);

    if query.feeds.is_some() {
        let feeds = crate::db::repo::list_feeds(pool).await.unwrap_or_default();
        response["feeds"] = serde_json::json!(feeds);
        response["feeds_groups"] = serde_json::json!([]);
    }

    if query.favicons.is_some() {
        let favicons = crate::db::repo::get_favicons(pool)
            .await
            .unwrap_or_default();
        response["favicons"] = serde_json::json!(favicons);
    }

    if query.items.is_some() {
        let items = if let Some(since_id) = query.since_id {
            crate::db::repo::get_items_since(pool, since_id, 50)
                .await
                .unwrap_or_default()
        } else if let Some(max_id) = query.max_id {
            crate::db::repo::get_items_before(pool, max_id, 50)
                .await
                .unwrap_or_default()
        } else if let Some(ref with_ids) = query.with_ids {
            let ids: Vec<i64> = with_ids
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            crate::db::repo::get_items_by_ids(pool, &ids)
                .await
                .unwrap_or_default()
        } else {
            crate::db::repo::get_items_since(pool, 0, 50)
                .await
                .unwrap_or_default()
        };
        let total = crate::db::repo::get_total_items(pool).await.unwrap_or(0);
        response["items"] = serde_json::json!(items);
        response["total_items"] = serde_json::json!(total);
    }

    if query.unread_item_ids.is_some() {
        let ids = crate::db::repo::get_unread_item_ids(pool)
            .await
            .unwrap_or_default();
        let ids_str: String = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        response["unread_item_ids"] = serde_json::json!(ids_str);
    }

    if query.saved_item_ids.is_some() {
        let ids = crate::db::repo::get_saved_item_ids(pool)
            .await
            .unwrap_or_default();
        let ids_str: String = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        response["saved_item_ids"] = serde_json::json!(ids_str);
    }

    if let (Some(mark), Some(action), Some(id)) = (&form.mark, &form.as_action, form.id) {
        match (mark.as_str(), action.as_str()) {
            ("item", "read") => {
                let _ = crate::db::repo::mark_item(pool, id, "is_read", 1).await;
                response["unread_item_ids"] = sync_unread_ids(pool).await;
            }
            ("item", "saved") => {
                let _ = crate::db::repo::mark_item(pool, id, "is_saved", 1).await;
                response["saved_item_ids"] = sync_saved_ids(pool).await;
            }
            ("item", "unsaved") => {
                let _ = crate::db::repo::mark_item(pool, id, "is_saved", 0).await;
                response["saved_item_ids"] = sync_saved_ids(pool).await;
            }
            ("feed", "read") => {
                let before = form.before.unwrap_or(0);
                let _ = crate::db::repo::mark_feed_read(pool, id, before).await;
                response["unread_item_ids"] = sync_unread_ids(pool).await;
            }
            _ => {}
        }
    }

    if form.unread_recently_read == Some(1) {
        let _ = crate::db::repo::unread_recently_read(pool).await;
        response["unread_item_ids"] = sync_unread_ids(pool).await;
    }

    Json(response)
}

async fn sync_unread_ids(pool: &sqlx::SqlitePool) -> serde_json::Value {
    let ids = crate::db::repo::get_unread_item_ids(pool)
        .await
        .unwrap_or_default();
    serde_json::json!(
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

async fn sync_saved_ids(pool: &sqlx::SqlitePool) -> serde_json::Value {
    let ids = crate::db::repo::get_saved_item_ids(pool)
        .await
        .unwrap_or_default();
    serde_json::json!(
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::Request;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        AppState::new(pool, format!("{:x}", md5::compute("test@test.com:pass")))
    }

    fn post_request(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn response_json(app: axum::Router, req: Request<Body>) -> serde_json::Value {
        let response = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn valid_key() -> String {
        format!("api_key={:x}", md5::compute("test@test.com:pass"))
    }

    #[tokio::test]
    async fn unauthenticated_request() {
        let state = test_state().await;
        let app = crate::api::router(state);
        let req = post_request("/?api", "api_key=wrong");
        let json = response_json(app, req).await;

        assert_eq!(json["api_version"], 3);
        assert_eq!(json["auth"], 0);
    }

    #[tokio::test]
    async fn missing_api_key() {
        let state = test_state().await;
        let app = crate::api::router(state);
        let req = post_request("/?api", "");
        let json = response_json(app, req).await;

        assert_eq!(json["api_version"], 3);
        assert_eq!(json["auth"], 0);
    }

    #[tokio::test]
    async fn authenticated_base_request() {
        let state = test_state().await;
        let app = crate::api::router(state);
        let req = post_request("/?api", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["api_version"], 3);
        assert_eq!(json["auth"], 1);
        assert!(json.get("last_refreshed_on_time").is_some());
    }

    #[tokio::test]
    async fn feeds_endpoint() {
        let state = test_state().await;
        crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let req = post_request("/?api&feeds", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        let feeds = json["feeds"].as_array().unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0]["url"], "https://example.com/feed");
        assert!(json.get("feeds_groups").is_some());
    }

    #[tokio::test]
    async fn favicons_endpoint() {
        let state = test_state().await;
        crate::db::repo::upsert_favicon(&state.pool, "image/gif;base64,AAAA")
            .await
            .unwrap();

        let app = crate::api::router(state);
        let req = post_request("/?api&favicons", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        let favicons = json["favicons"].as_array().unwrap();
        assert_eq!(favicons.len(), 1);
        assert_eq!(favicons[0]["data"], "image/gif;base64,AAAA");
    }

    #[tokio::test]
    async fn items_endpoint_default() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        for i in 1..=3 {
            crate::db::repo::insert_item(
                &state.pool,
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

        let app = crate::api::router(state);
        let req = post_request("/?api&items", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(json["total_items"], 3);
    }

    #[tokio::test]
    async fn items_with_since_id() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let mut ids = vec![];
        for i in 1..=5 {
            let item = crate::db::repo::insert_item(
                &state.pool,
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

        let app = crate::api::router(state);
        let uri = format!("/?api&items&since_id={}", ids[2]);
        let req = post_request(&uri, &valid_key());
        let json = response_json(app, req).await;

        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn items_with_max_id() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let mut ids = vec![];
        for i in 1..=5 {
            let item = crate::db::repo::insert_item(
                &state.pool,
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

        let app = crate::api::router(state);
        let uri = format!("/?api&items&max_id={}", ids[2]);
        let req = post_request(&uri, &valid_key());
        let json = response_json(app, req).await;

        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn items_with_ids() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let mut ids = vec![];
        for i in 1..=5 {
            let item = crate::db::repo::insert_item(
                &state.pool,
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

        let app = crate::api::router(state);
        let uri = format!("/?api&items&with_ids={},{}", ids[0], ids[4]);
        let req = post_request(&uri, &valid_key());
        let json = response_json(app, req).await;

        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn unread_item_ids_endpoint() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let item1 = crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "A",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();
        let item2 = crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "B",
            "",
            "",
            "https://example.com/2",
            1700000001,
        )
        .await
        .unwrap();
        crate::db::repo::mark_item(&state.pool, item1.id, "is_read", 1)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let req = post_request("/?api&unread_item_ids", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["unread_item_ids"], format!("{}", item2.id));
    }

    #[tokio::test]
    async fn saved_item_ids_endpoint() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let item1 = crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "A",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();
        crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "B",
            "",
            "",
            "https://example.com/2",
            1700000001,
        )
        .await
        .unwrap();
        crate::db::repo::mark_item(&state.pool, item1.id, "is_saved", 1)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let req = post_request("/?api&saved_item_ids", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["saved_item_ids"], format!("{}", item1.id));
    }

    #[tokio::test]
    async fn combined_query_params() {
        let state = test_state().await;
        crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let req = post_request("/?api&feeds&unread_item_ids", &valid_key());
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert!(json.get("feeds").is_some());
        assert!(json.get("unread_item_ids").is_some());
    }

    async fn seed_item(
        pool: &sqlx::SqlitePool,
    ) -> (crate::db::models::Feed, crate::db::models::Item) {
        let feed = crate::db::repo::insert_feed(pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let item = crate::db::repo::insert_item(
            pool,
            feed.id,
            "Article",
            "",
            "",
            "https://example.com/1",
            1700000000,
        )
        .await
        .unwrap();
        (feed, item)
    }

    #[tokio::test]
    async fn mark_item_as_read() {
        let state = test_state().await;
        let (_feed, item) = seed_item(&state.pool).await;

        let app = crate::api::router(state);
        let body = format!("{}&mark=item&as=read&id={}", valid_key(), item.id);
        let req = post_request("/?api", &body);
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["unread_item_ids"], "");
    }

    #[tokio::test]
    async fn mark_item_as_saved() {
        let state = test_state().await;
        let (_feed, item) = seed_item(&state.pool).await;

        let app = crate::api::router(state);
        let body = format!("{}&mark=item&as=saved&id={}", valid_key(), item.id);
        let req = post_request("/?api", &body);
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["saved_item_ids"], format!("{}", item.id));
    }

    #[tokio::test]
    async fn mark_item_as_unsaved() {
        let state = test_state().await;
        let (_feed, item) = seed_item(&state.pool).await;
        crate::db::repo::mark_item(&state.pool, item.id, "is_saved", 1)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let body = format!("{}&mark=item&as=unsaved&id={}", valid_key(), item.id);
        let req = post_request("/?api", &body);
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["saved_item_ids"], "");
    }

    #[tokio::test]
    async fn mark_feed_as_read() {
        let state = test_state().await;
        let feed = crate::db::repo::insert_feed(&state.pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        let item1 = crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "Old",
            "",
            "",
            "https://example.com/1",
            1000,
        )
        .await
        .unwrap();
        let item2 = crate::db::repo::insert_item(
            &state.pool,
            feed.id,
            "New",
            "",
            "",
            "https://example.com/2",
            2000,
        )
        .await
        .unwrap();

        let app = crate::api::router(state);
        let body = format!(
            "{}&mark=feed&as=read&id={}&before=1500",
            valid_key(),
            feed.id
        );
        let req = post_request("/?api", &body);
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        let unread = json["unread_item_ids"].as_str().unwrap();
        assert!(!unread.contains(&item1.id.to_string()));
        assert!(unread.contains(&item2.id.to_string()));
    }

    #[tokio::test]
    async fn unread_recently_read() {
        let state = test_state().await;
        let (_feed, item) = seed_item(&state.pool).await;
        crate::db::repo::mark_item(&state.pool, item.id, "is_read", 1)
            .await
            .unwrap();

        let app = crate::api::router(state);
        let body = format!("{}&unread_recently_read=1", valid_key());
        let req = post_request("/?api", &body);
        let json = response_json(app, req).await;

        assert_eq!(json["auth"], 1);
        assert_eq!(json["unread_item_ids"], format!("{}", item.id));
    }
}
