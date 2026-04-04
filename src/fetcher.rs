use std::time::Duration;

use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;
const MAX_FAVICON_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;

pub fn build_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

pub fn map_entry(entry: &feed_rs::model::Entry) -> (String, String, String, String, i64) {
    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_default();
    let author = entry
        .authors
        .first()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    let html = entry
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()))
        .unwrap_or_default();
    let url = entry
        .links
        .first()
        .map(|l| l.href.clone())
        .unwrap_or_default();
    let created_on_time = entry
        .published
        .or(entry.updated)
        .map(|dt| dt.timestamp())
        .unwrap_or(0);
    (title, author, html, url, created_on_time)
}

pub fn extract_site_url(feed: &feed_rs::model::Feed) -> String {
    feed.links
        .iter()
        .find(|l| l.rel.as_deref() == Some("alternate"))
        .or_else(|| feed.links.first())
        .map(|l| l.href.clone())
        .unwrap_or_default()
}

pub fn extract_feed_title(feed: &feed_rs::model::Feed) -> String {
    feed.title
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_default()
}

pub async fn process_feed(
    pool: &SqlitePool,
    feed_id: i64,
    body: &[u8],
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let parsed = feed_rs::parser::parse(body)?;

    let title = extract_feed_title(&parsed);
    let site_url = extract_site_url(&parsed);
    if !title.is_empty() || !site_url.is_empty() {
        let _ = crate::db::repo::update_feed_title_and_site(pool, feed_id, &title, &site_url).await;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let _ = crate::db::repo::update_feed_last_updated(pool, feed_id, now).await;

    let mut inserted = 0;
    for entry in &parsed.entries {
        let (title, author, html, url, created_on_time) = map_entry(entry);
        if url.is_empty() {
            continue;
        }
        let exists = crate::db::repo::item_exists_by_url(pool, feed_id, &url)
            .await
            .unwrap_or(true);
        if exists {
            continue;
        }
        if crate::db::repo::insert_item(
            pool,
            feed_id,
            &title,
            &author,
            &html,
            &url,
            created_on_time,
        )
        .await
        .is_ok()
        {
            inserted += 1;
        }
    }

    Ok(inserted)
}

pub fn is_feed_due(
    last_updated: i64,
    interval_minutes: i64,
    consecutive_failures: i64,
    now: i64,
) -> bool {
    if last_updated == 0 {
        return true;
    }
    let backoff = 2i64.pow(consecutive_failures.min(6) as u32);
    let effective_interval = interval_minutes * 60 * backoff;
    let elapsed = now - last_updated;
    elapsed >= effective_interval
}

const FAVICON_REFRESH_SECS: i64 = 7 * 24 * 3600;

pub fn should_refresh_favicon(favicon_id: i64, favicon_last_checked: i64, now: i64) -> bool {
    if favicon_id == 0 {
        return true;
    }
    if favicon_last_checked == 0 {
        return true;
    }
    now - favicon_last_checked >= FAVICON_REFRESH_SECS
}

pub async fn fetch_feed(
    client: &reqwest::Client,
    url: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let body = read_limited(resp, MAX_FEED_BYTES).await?;
    Ok(body)
}

async fn read_limited(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(len) = resp.content_length()
        && len as usize > max_bytes
    {
        return Err(format!("response too large: {} bytes (max {})", len, max_bytes).into());
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "response too large: {} bytes (max {})",
            bytes.len(),
            max_bytes
        )
        .into());
    }
    Ok(bytes.to_vec())
}

pub fn extract_favicon_url(html: &str, base_url: &str) -> Option<String> {
    let lower = html.to_lowercase();
    for segment in lower.split('<') {
        if !segment.starts_with("link ")
            && !segment.starts_with("link\n")
            && !segment.starts_with("link\t")
        {
            continue;
        }
        if !segment.contains("icon") {
            continue;
        }
        if let Some(href_start) = segment.find("href=") {
            let rest = &segment[href_start + 5..];
            let (quote, rest) = if let Some(stripped) = rest.strip_prefix('"') {
                ('"', stripped)
            } else if let Some(stripped) = rest.strip_prefix('\'') {
                ('\'', stripped)
            } else {
                continue;
            };
            if let Some(end) = rest.find(quote) {
                let href = &rest[..end];
                return Some(resolve_url(href, base_url));
            }
        }
    }

    let base = base_url.trim_end_matches('/');
    Some(format!("{}/favicon.ico", base))
}

fn resolve_url(href: &str, base_url: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with("//") {
        let scheme = if base_url.starts_with("https") {
            "https:"
        } else {
            "http:"
        };
        return format!("{}{}", scheme, href);
    }
    let base = base_url.trim_end_matches('/');
    let href = href.trim_start_matches('/');
    format!("{}/{}", base, href)
}

pub async fn fetch_and_store_favicon(
    client: &reqwest::Client,
    pool: &SqlitePool,
    feed_id: i64,
    site_url: &str,
    existing_favicon_id: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let html_resp = client.get(site_url).send().await?.error_for_status()?;
    let html_bytes = read_limited(html_resp, MAX_HTML_BYTES).await?;
    let html = String::from_utf8_lossy(&html_bytes);

    let favicon_url = extract_favicon_url(&html, site_url).ok_or("no favicon url")?;

    let stored_etag = if existing_favicon_id > 0 {
        crate::db::repo::get_favicon(pool, existing_favicon_id)
            .await
            .ok()
            .flatten()
            .map(|f| f.etag)
            .unwrap_or_default()
    } else {
        String::new()
    };

    let mut req = client.get(&favicon_url);
    if !stored_etag.is_empty() {
        req = req.header("If-None-Match", &stored_etag);
    }

    let icon_resp = req.send().await?.error_for_status()?;

    if icon_resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let _ = crate::db::repo::update_favicon_last_checked(pool, feed_id, now).await;
        return Ok(());
    }

    let content_type = icon_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/x-icon")
        .to_string();
    let new_etag = icon_resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let icon_bytes = read_limited(icon_resp, MAX_FAVICON_BYTES).await?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&icon_bytes);
    let data = format!("{};base64,{}", content_type, b64);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if existing_favicon_id > 0 {
        let _ = crate::db::repo::update_favicon_data_and_etag(
            pool,
            existing_favicon_id,
            &data,
            &new_etag,
        )
        .await;
    } else {
        let favicon = crate::db::repo::upsert_favicon_with_etag(pool, &data, &new_etag).await?;
        crate::db::repo::update_feed_favicon(pool, feed_id, favicon.id).await?;
    }
    let _ = crate::db::repo::update_favicon_last_checked(pool, feed_id, now).await;

    Ok(())
}

const SCHEDULER_TICK: Duration = Duration::from_secs(60);

pub async fn check_due_feeds(
    pool: &SqlitePool,
    now: i64,
) -> Result<Vec<crate::db::models::Feed>, sqlx::Error> {
    let feeds = crate::db::repo::list_feeds(pool).await?;
    Ok(feeds
        .into_iter()
        .filter(|f| {
            is_feed_due(
                f.last_updated_on_time,
                f.fetch_interval_minutes,
                f.consecutive_failures,
                now,
            )
        })
        .collect())
}

pub async fn fetch_one_feed(
    client: &reqwest::Client,
    pool: &SqlitePool,
    feed: &crate::db::models::Feed,
) {
    tracing::info!(feed_id = feed.id, url = %feed.url, "fetching feed");

    let success = match fetch_feed(client, &feed.url).await {
        Ok(body) => match process_feed(pool, feed.id, &body).await {
            Ok(count) => {
                tracing::info!(feed_id = feed.id, new_items = count, "feed processed");
                true
            }
            Err(e) => {
                tracing::warn!(feed_id = feed.id, error = %e, "failed to process feed");
                false
            }
        },
        Err(e) => {
            tracing::warn!(feed_id = feed.id, error = %e, "failed to fetch feed");
            false
        }
    };

    if success {
        let _ = crate::db::repo::reset_failures(pool, feed.id).await;
    } else {
        let _ = crate::db::repo::increment_failures(pool, feed.id).await;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if !feed.site_url.is_empty()
        && should_refresh_favicon(feed.favicon_id, feed.favicon_last_checked, now)
        && let Err(e) =
            fetch_and_store_favicon(client, pool, feed.id, &feed.site_url, feed.favicon_id).await
    {
        tracing::warn!(feed_id = feed.id, error = %e, "failed to fetch favicon");
    }
}

pub async fn run_scheduler(pool: SqlitePool, client: reqwest::Client, shutdown: CancellationToken) {
    let mut interval = tokio::time::interval(SCHEDULER_TICK);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("scheduler shutting down");
                return;
            }
            _ = interval.tick() => {}
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let due = match check_due_feeds(&pool, now).await {
            Ok(feeds) => feeds,
            Err(e) => {
                tracing::warn!(error = %e, "failed to check due feeds");
                continue;
            }
        };

        if due.is_empty() {
            continue;
        }

        tracing::info!(count = due.len(), "feeds due for refresh");

        for feed in &due {
            fetch_one_feed(&client, &pool, feed).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rss() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Example Blog</title>
    <link>https://example.com</link>
    <item>
      <title>First Post</title>
      <link>https://example.com/first</link>
      <description>Hello world</description>
      <author>alice@example.com (Alice)</author>
      <pubDate>Sat, 01 Jan 2025 00:00:00 GMT</pubDate>
    </item>
    <item>
      <title>Second Post</title>
      <link>https://example.com/second</link>
      <description>Another post</description>
      <pubDate>Sun, 02 Jan 2025 00:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#
    }

    fn sample_atom() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Feed</title>
  <link href="https://atom.example.com" rel="alternate"/>
  <entry>
    <title>Entry One</title>
    <link href="https://atom.example.com/one"/>
    <content type="html">&lt;p&gt;Rich content&lt;/p&gt;</content>
    <author><name>Bob</name></author>
    <published>2025-01-01T00:00:00Z</published>
  </entry>
</feed>"#
    }

    #[test]
    fn map_entry_from_rss() {
        let parsed = feed_rs::parser::parse(sample_rss()).unwrap();
        let (title, _author, html, url, ts) = map_entry(&parsed.entries[0]);
        assert_eq!(title, "First Post");
        assert_eq!(url, "https://example.com/first");
        assert_eq!(html, "Hello world");
        assert!(ts > 0);
    }

    #[test]
    fn map_entry_from_atom() {
        let parsed = feed_rs::parser::parse(sample_atom()).unwrap();
        let (title, author, html, url, _ts) = map_entry(&parsed.entries[0]);
        assert_eq!(title, "Entry One");
        assert_eq!(author, "Bob");
        assert!(html.contains("Rich content"));
        assert_eq!(url, "https://atom.example.com/one");
    }

    #[test]
    fn extract_site_url_from_rss() {
        let parsed = feed_rs::parser::parse(sample_rss()).unwrap();
        let site = extract_site_url(&parsed);
        assert_eq!(site, "https://example.com/");
    }

    #[test]
    fn extract_site_url_from_atom() {
        let parsed = feed_rs::parser::parse(sample_atom()).unwrap();
        let site = extract_site_url(&parsed);
        assert_eq!(site, "https://atom.example.com/");
    }

    #[test]
    fn extract_feed_title_from_rss() {
        let parsed = feed_rs::parser::parse(sample_rss()).unwrap();
        assert_eq!(extract_feed_title(&parsed), "Example Blog");
    }

    #[tokio::test]
    async fn process_feed_inserts_items() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let inserted = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(inserted, 2);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.title, "Example Blog");
        assert_eq!(feed.site_url, "https://example.com/");
        assert!(feed.last_updated_on_time > 0);
    }

    #[tokio::test]
    async fn process_feed_deduplicates_items() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed.xml", 60)
            .await
            .unwrap();

        let first = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(first, 2);

        let second = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(second, 0);
    }

    #[test]
    fn is_feed_due_never_fetched() {
        assert!(is_feed_due(0, 60, 0, 1000));
    }

    #[test]
    fn is_feed_due_recently_fetched() {
        let now = 1_700_000_000;
        let last = now - 1800;
        assert!(!is_feed_due(last, 60, 0, now));
    }

    #[test]
    fn is_feed_due_interval_elapsed() {
        let now = 1_700_000_000;
        let last = now - 3601;
        assert!(is_feed_due(last, 60, 0, now));
    }

    #[test]
    fn extract_favicon_url_from_html() {
        let html = r#"<html><head><link rel="icon" href="/img/favicon.png"></head></html>"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/img/favicon.png");
    }

    #[test]
    fn extract_favicon_url_absolute() {
        let html = r#"<link rel="shortcut icon" href="https://cdn.example.com/icon.ico">"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://cdn.example.com/icon.ico");
    }

    #[test]
    fn extract_favicon_url_fallback() {
        let html = r#"<html><head><title>No icon</title></head></html>"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/favicon.ico");
    }

    #[tokio::test]
    async fn process_feed_atom_format() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://atom.example.com/feed", 60)
            .await
            .unwrap();

        let inserted = process_feed(&pool, feed.id, sample_atom()).await.unwrap();
        assert_eq!(inserted, 1);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.title, "Atom Feed");
        assert_eq!(feed.site_url, "https://atom.example.com/");
    }

    #[tokio::test]
    async fn process_feed_garbage_bytes() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let result = process_feed(&pool, feed.id, b"\x00\x01\x02\xff garbage").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn process_feed_empty_body() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let result = process_feed(&pool, feed.id, b"").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn process_feed_html_not_a_feed() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let html = b"<html><body><h1>Not a feed</h1></body></html>";
        let result = process_feed(&pool, feed.id, html).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn process_feed_valid_xml_not_a_feed() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let xml = br#"<?xml version="1.0"?><catalog><book>Title</book></catalog>"#;
        let result = process_feed(&pool, feed.id, xml).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn process_feed_no_entries() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Empty</title><link>https://example.com</link></channel></rss>"#;
        let inserted = process_feed(&pool, feed.id, xml).await.unwrap();
        assert_eq!(inserted, 0);

        let updated = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.title, "Empty");
        assert!(updated.last_updated_on_time > 0);
    }

    #[tokio::test]
    async fn process_feed_entries_missing_urls_skipped() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Blog</title><link>https://example.com</link>
<item><title>No Link Entry</title><description>Has no link</description></item>
<item><title>Has Link</title><link>https://example.com/real</link><description>Content</description></item>
</channel></rss>"#;
        let inserted = process_feed(&pool, feed.id, xml).await.unwrap();
        assert_eq!(inserted, 1);
    }

    #[tokio::test]
    async fn process_feed_entries_missing_metadata_uses_defaults() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Blog</title><link>https://example.com</link>
<item><link>https://example.com/bare</link></item>
</channel></rss>"#;
        let inserted = process_feed(&pool, feed.id, xml).await.unwrap();
        assert_eq!(inserted, 1);

        let items = crate::db::repo::get_items_since(&pool, 0, 50)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "");
        assert_eq!(items[0].author, "");
        assert_eq!(items[0].html, "");
        assert_eq!(items[0].created_on_time, 0);
    }

    #[tokio::test]
    async fn process_feed_mixed_valid_and_broken_entries() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>Mixed</title><link>https://example.com</link>
<item><title>Good</title><link>https://example.com/good</link><description>OK</description></item>
<item><title>No URL</title><description>Skipped</description></item>
<item><title>Also Good</title><link>https://example.com/also</link><description>Fine</description></item>
</channel></rss>"#;
        let inserted = process_feed(&pool, feed.id, xml).await.unwrap();
        assert_eq!(inserted, 2);
    }

    #[test]
    fn map_entry_completely_empty() {
        let entry = feed_rs::model::Entry {
            id: String::new(),
            title: None,
            updated: None,
            authors: vec![],
            content: None,
            links: vec![],
            summary: None,
            categories: vec![],
            contributors: vec![],
            published: None,
            source: None,
            rights: None,
            media: vec![],
            language: None,
            base: None,
        };
        let (title, author, html, url, ts) = map_entry(&entry);
        assert_eq!(title, "");
        assert_eq!(author, "");
        assert_eq!(html, "");
        assert_eq!(url, "");
        assert_eq!(ts, 0);
    }

    #[test]
    fn map_entry_uses_summary_when_no_content() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>T</title><link>https://x.com</link>
<item><title>T</title><link>https://x.com/1</link><description>Summary only</description></item>
</channel></rss>"#;
        let parsed = feed_rs::parser::parse(xml as &[u8]).unwrap();
        let (_title, _author, html, _url, _ts) = map_entry(&parsed.entries[0]);
        assert_eq!(html, "Summary only");
    }

    #[test]
    fn map_entry_prefers_content_over_summary() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>T</title><link href="https://x.com" rel="alternate"/>
  <entry>
    <title>T</title><link href="https://x.com/1"/>
    <content type="html">Full content</content>
    <summary>Just a summary</summary>
    <published>2025-01-01T00:00:00Z</published>
  </entry>
</feed>"#;
        let parsed = feed_rs::parser::parse(xml as &[u8]).unwrap();
        let (_title, _author, html, _url, _ts) = map_entry(&parsed.entries[0]);
        assert_eq!(html, "Full content");
    }

    #[test]
    fn map_entry_uses_updated_when_no_published() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>T</title><link href="https://x.com" rel="alternate"/>
  <entry>
    <title>T</title><link href="https://x.com/1"/>
    <updated>2025-06-15T12:00:00Z</updated>
  </entry>
</feed>"#;
        let parsed = feed_rs::parser::parse(xml as &[u8]).unwrap();
        let (_title, _author, _html, _url, ts) = map_entry(&parsed.entries[0]);
        assert!(ts > 0);
    }

    #[test]
    fn is_feed_due_future_last_updated() {
        let now = 1_700_000_000;
        let last = now + 3600;
        assert!(!is_feed_due(last, 60, 0, now));
    }

    #[test]
    fn is_feed_due_zero_interval() {
        let now = 1_700_000_000;
        let last = now - 1;
        assert!(is_feed_due(last, 0, 0, now));
    }

    #[test]
    fn is_feed_due_exact_boundary() {
        let now = 1_700_000_000;
        let last = now - 3600;
        assert!(is_feed_due(last, 60, 0, now));
    }

    #[test]
    fn extract_favicon_url_empty_href() {
        let html = r#"<link rel="icon" href="">"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/");
    }

    #[test]
    fn extract_favicon_url_missing_quotes() {
        let html = r#"<link rel="icon" href=favicon.ico>"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/favicon.ico");
    }

    #[test]
    fn extract_favicon_url_multiple_icons_picks_first() {
        let html = r#"<link rel="icon" href="/first.png"><link rel="icon" href="/second.png">"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/first.png");
    }

    #[test]
    fn extract_favicon_url_protocol_relative() {
        let html = r#"<link rel="icon" href="//cdn.example.com/icon.png">"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert!(url.contains("cdn.example.com/icon.png"));
    }

    #[test]
    fn extract_favicon_url_empty_base_url() {
        let html = r#"<html><head><title>No icon</title></head></html>"#;
        let url = extract_favicon_url(html, "").unwrap();
        assert_eq!(url, "/favicon.ico");
    }

    #[test]
    fn extract_favicon_url_single_quotes() {
        let html = r#"<link rel='icon' href='/icon.png'>"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/icon.png");
    }

    #[test]
    fn extract_favicon_url_apple_touch_icon() {
        let html =
            r#"<link rel="apple-touch-icon" href="/apple.png"><link rel="icon" href="/real.png">"#;
        let url = extract_favicon_url(html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/apple.png");
    }

    #[test]
    fn extract_favicon_url_large_html_does_not_hang() {
        let mut html = String::from("<html><head>");
        for _ in 0..10_000 {
            html.push_str("<meta name=\"viewport\" content=\"width=device-width\">");
        }
        html.push_str("</head><body>");
        for _ in 0..10_000 {
            html.push_str("<p>Lorem ipsum dolor sit amet</p>");
        }
        html.push_str("</body></html>");
        let url = extract_favicon_url(&html, "https://example.com").unwrap();
        assert_eq!(url, "https://example.com/favicon.ico");
    }

    #[tokio::test]
    async fn fetch_feed_respects_size_limit() {
        let client = build_client();
        assert!(client.is_ok());
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn check_due_feeds_returns_due_only() {
        let pool = test_pool().await;
        let now = 1_700_000_000i64;

        let f1 = crate::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_feed_last_updated(&pool, f1.id, now - 7200)
            .await
            .unwrap();

        let f2 = crate::db::repo::insert_feed(&pool, "https://b.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_feed_last_updated(&pool, f2.id, now - 1800)
            .await
            .unwrap();

        let due = check_due_feeds(&pool, now).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, f1.id);
    }

    #[tokio::test]
    async fn check_due_feeds_includes_never_fetched() {
        let pool = test_pool().await;
        let now = 1_700_000_000i64;

        crate::db::repo::insert_feed(&pool, "https://new.com/feed", 60)
            .await
            .unwrap();

        let due = check_due_feeds(&pool, now).await.unwrap();
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn check_due_feeds_empty_when_none_due() {
        let pool = test_pool().await;
        let now = 1_700_000_000i64;

        let f = crate::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_feed_last_updated(&pool, f.id, now - 100)
            .await
            .unwrap();

        let due = check_due_feeds(&pool, now).await.unwrap();
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn check_due_feeds_empty_db() {
        let pool = test_pool().await;
        let due = check_due_feeds(&pool, 1_700_000_000).await.unwrap();
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn check_due_feeds_respects_backoff() {
        let pool = test_pool().await;
        let now = 1_700_000_000i64;

        let f = crate::db::repo::insert_feed(&pool, "https://fail.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_feed_last_updated(&pool, f.id, now - 7200)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();

        let due = check_due_feeds(&pool, now).await.unwrap();
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn check_due_feeds_backoff_eventually_due() {
        let pool = test_pool().await;
        let now = 1_700_000_000i64;

        let f = crate::db::repo::insert_feed(&pool, "https://fail.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();

        crate::db::repo::update_feed_last_updated(&pool, f.id, now - 7201)
            .await
            .unwrap();

        let due = check_due_feeds(&pool, now).await.unwrap();
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn reset_failures_clears_count() {
        let pool = test_pool().await;
        let f = crate::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();
        crate::db::repo::increment_failures(&pool, f.id)
            .await
            .unwrap();
        crate::db::repo::reset_failures(&pool, f.id).await.unwrap();

        let feed = crate::db::repo::get_feed(&pool, f.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.consecutive_failures, 0);
    }

    #[test]
    fn is_feed_due_with_backoff_one_failure() {
        let now = 1_700_000_000;
        let last = now - 7200;
        assert!(is_feed_due(last, 60, 1, now));
    }

    #[test]
    fn is_feed_due_with_backoff_delays_retry() {
        let now = 1_700_000_000;
        let last = now - 3700;
        assert!(!is_feed_due(last, 60, 1, now));
    }

    #[test]
    fn is_feed_due_backoff_capped() {
        let now = 1_700_000_000;
        let last = now - (60 * 64 * 60 + 1);
        assert!(is_feed_due(last, 60, 10, now));

        let last_too_recent = now - (60 * 64 * 60 - 1);
        assert!(!is_feed_due(last_too_recent, 60, 10, now));
    }

    #[test]
    fn should_refresh_favicon_no_favicon() {
        assert!(should_refresh_favicon(0, 0, 1_700_000_000));
    }

    #[test]
    fn should_refresh_favicon_recent_check() {
        let now = 1_700_000_000;
        let checked = now - 3600;
        assert!(!should_refresh_favicon(1, checked, now));
    }

    #[test]
    fn should_refresh_favicon_stale_check() {
        let now = 1_700_000_000;
        let checked = now - (8 * 24 * 3600);
        assert!(should_refresh_favicon(1, checked, now));
    }

    #[test]
    fn should_refresh_favicon_never_checked_but_has_favicon() {
        assert!(should_refresh_favicon(1, 0, 1_700_000_000));
    }

    #[tokio::test]
    async fn upsert_favicon_with_etag() {
        let pool = test_pool().await;
        let fav = crate::db::repo::upsert_favicon_with_etag(
            &pool,
            "data:image/png;base64,AAA",
            "etag-123",
        )
        .await
        .unwrap();
        assert_eq!(fav.etag, "etag-123");
        assert_eq!(fav.data, "data:image/png;base64,AAA");
    }

    #[tokio::test]
    async fn update_favicon_data_and_etag() {
        let pool = test_pool().await;
        let fav = crate::db::repo::upsert_favicon(&pool, "old-data")
            .await
            .unwrap();
        crate::db::repo::update_favicon_data_and_etag(&pool, fav.id, "new-data", "new-etag")
            .await
            .unwrap();
        let favicons = crate::db::repo::get_favicons(&pool).await.unwrap();
        assert_eq!(favicons[0].data, "new-data");
        assert_eq!(favicons[0].etag, "new-etag");
    }

    #[tokio::test]
    async fn update_favicon_last_checked() {
        let pool = test_pool().await;
        let f = crate::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_favicon_last_checked(&pool, f.id, 1_700_000_000)
            .await
            .unwrap();
        let feed = crate::db::repo::get_feed(&pool, f.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.favicon_last_checked, 1_700_000_000);
    }

    #[tokio::test]
    async fn scheduler_stops_on_shutdown() {
        let pool = test_pool().await;
        let client = build_client().unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let cancel = token.clone();

        let handle = tokio::spawn(run_scheduler(pool, client, token));
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "scheduler did not stop within timeout");
    }
}
