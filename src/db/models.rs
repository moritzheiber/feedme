use serde::Serialize;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Feed {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub site_url: String,
    pub favicon_id: i64,
    pub is_spark: i64,
    pub last_updated_on_time: i64,
    #[serde(skip)]
    pub fetch_interval_minutes: i64,
    #[serde(skip)]
    pub feed_ttl_minutes: i64,
    #[serde(skip)]
    pub skip_hours_mask: i64,
    #[serde(skip)]
    pub skip_days_mask: i64,
    #[serde(skip)]
    pub consecutive_failures: i64,
    #[serde(skip)]
    pub favicon_last_checked: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Item {
    pub id: i64,
    pub feed_id: i64,
    pub title: String,
    pub author: String,
    pub html: String,
    pub url: String,
    pub is_saved: i64,
    pub is_read: i64,
    pub created_on_time: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Favicon {
    pub id: i64,
    pub data: String,
    #[serde(skip)]
    pub etag: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_serializes_to_fever_json() {
        let feed = Feed {
            id: 1,
            title: "Test Feed".to_string(),
            url: "https://example.com/feed.xml".to_string(),
            site_url: "https://example.com".to_string(),
            favicon_id: 5,
            is_spark: 0,
            last_updated_on_time: 1700000000,
            fetch_interval_minutes: 60,
            feed_ttl_minutes: 0,
            skip_hours_mask: 0,
            skip_days_mask: 0,
            consecutive_failures: 0,
            favicon_last_checked: 0,
        };

        let json: serde_json::Value = serde_json::to_value(&feed).unwrap();
        assert_eq!(json["id"], 1);
        assert_eq!(json["title"], "Test Feed");
        assert_eq!(json["url"], "https://example.com/feed.xml");
        assert_eq!(json["site_url"], "https://example.com");
        assert_eq!(json["favicon_id"], 5);
        assert_eq!(json["is_spark"], 0);
        assert_eq!(json["last_updated_on_time"], 1700000000);
        assert!(json.get("fetch_interval_minutes").is_none());
    }

    #[test]
    fn item_serializes_to_fever_json() {
        let item = Item {
            id: 42,
            feed_id: 1,
            title: "Article".to_string(),
            author: "Author".to_string(),
            html: "<p>Content</p>".to_string(),
            url: "https://example.com/article".to_string(),
            is_saved: 0,
            is_read: 1,
            created_on_time: 1700000000,
        };

        let json: serde_json::Value = serde_json::to_value(&item).unwrap();
        assert_eq!(json["id"], 42);
        assert_eq!(json["feed_id"], 1);
        assert_eq!(json["title"], "Article");
        assert_eq!(json["author"], "Author");
        assert_eq!(json["html"], "<p>Content</p>");
        assert_eq!(json["url"], "https://example.com/article");
        assert_eq!(json["is_saved"], 0);
        assert_eq!(json["is_read"], 1);
        assert_eq!(json["created_on_time"], 1700000000);
    }

    #[test]
    fn favicon_serializes_to_fever_json() {
        let favicon = Favicon {
            id: 5,
            data: "image/gif;base64,R0lGODlhAQABAIAAAObm5gAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw=="
                .to_string(),
            etag: String::new(),
        };

        let json: serde_json::Value = serde_json::to_value(&favicon).unwrap();
        assert_eq!(json["id"], 5);
        assert!(
            json["data"]
                .as_str()
                .unwrap()
                .starts_with("image/gif;base64,")
        );
    }
}
