use std::time::Duration;

use feedparser_rs::{FeedMeta, UpdatePeriod};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;
const MAX_FAVICON_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const USER_AGENT: &str = concat!("feedme/", env!("CARGO_PKG_VERSION"));
const FEED_ACCEPT: &str =
    "application/rss+xml, application/atom+xml, application/xml;q=0.9, text/xml;q=0.8, */*;q=0.1";

pub fn build_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

pub struct ProcessResult {
    pub inserted: usize,
    pub icon_url: Option<String>,
}

fn extract_entry_url(entry: &feedparser_rs::Entry) -> String {
    entry.link.as_deref().unwrap_or_default().to_string()
}

fn extract_entry_title(entry: &feedparser_rs::Entry) -> String {
    entry.title.clone().unwrap_or_default()
}

fn extract_entry_author(entry: &feedparser_rs::Entry, feed: &FeedMeta) -> String {
    entry
        .author
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| entry.author_detail.as_ref().and_then(|p| p.name.as_deref()))
        .or(entry.dc_creator.as_deref())
        .or(feed.author.as_deref())
        .or_else(|| feed.author_detail.as_ref().and_then(|p| p.name.as_deref()))
        .or(feed.dc_creator.as_deref())
        .unwrap_or_default()
        .to_string()
}

fn extract_entry_html(entry: &feedparser_rs::Entry) -> String {
    entry
        .content
        .first()
        .map(|c| c.value.clone())
        .or_else(|| entry.summary.clone())
        .unwrap_or_default()
}

fn extract_entry_timestamp(entry: &feedparser_rs::Entry) -> i64 {
    entry
        .published
        .or(entry.updated)
        .or(entry.created)
        .or(entry.dc_date)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

pub fn extract_site_url(feed: &FeedMeta) -> String {
    feed.links
        .iter()
        .find(|l| l.rel.as_deref() == Some("alternate"))
        .map(|l| l.href.to_string())
        .or_else(|| feed.link.as_deref().map(String::from))
        .or_else(|| feed.links.first().map(|l| l.href.to_string()))
        .unwrap_or_default()
}

pub fn extract_feed_title(feed: &FeedMeta) -> String {
    feed.title.clone().unwrap_or_default()
}

pub fn extract_feed_icon_url(feed: &FeedMeta) -> Option<String> {
    feed.icon
        .as_deref()
        .or(feed.logo.as_deref())
        .or(feed.image.as_ref().map(|img| img.url.as_str()))
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub fn extract_recommended_interval(feed: &FeedMeta) -> i64 {
    if let Some(ttl) = feed.ttl.as_deref().and_then(|s| s.parse::<i64>().ok())
        && ttl > 0
    {
        return ttl;
    }

    if let Some(syn) = &feed.syndication {
        let period_minutes: i64 = match syn.update_period {
            Some(UpdatePeriod::Hourly) => 60,
            Some(UpdatePeriod::Daily) => 1440,
            Some(UpdatePeriod::Weekly) => 10080,
            Some(UpdatePeriod::Monthly) => 43200,
            Some(UpdatePeriod::Yearly) => 525600,
            None => return 0,
        };
        let freq = syn
            .update_frequency
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(1)
            .max(1);
        return period_minutes / freq;
    }

    0
}

pub fn extract_skip_hours_mask(feed: &FeedMeta) -> i64 {
    let mut mask: i64 = 0;
    for &hour in &feed.skiphours {
        if hour < 24 {
            mask |= 1 << hour;
        }
    }
    mask
}

pub fn extract_skip_days_mask(feed: &FeedMeta) -> i64 {
    let mut mask: i64 = 0;
    for day in &feed.skipdays {
        let bit = match day.to_lowercase().as_str() {
            "sunday" => 0,
            "monday" => 1,
            "tuesday" => 2,
            "wednesday" => 3,
            "thursday" => 4,
            "friday" => 5,
            "saturday" => 6,
            _ => continue,
        };
        mask |= 1 << bit;
    }
    mask
}

pub async fn process_feed(
    pool: &SqlitePool,
    feed_id: i64,
    body: &[u8],
) -> Result<(usize, Option<String>), Box<dyn std::error::Error + Send + Sync>> {
    tracing::debug!(feed_id, body_size = body.len(), "parsing feed body");

    let parsed = feedparser_rs::parse(body)?;

    if parsed.bozo {
        tracing::warn!(
            feed_id,
            bozo_exception = parsed.bozo_exception.as_deref().unwrap_or("unknown"),
            "feed parsed with errors"
        );
    }

    let title = extract_feed_title(&parsed.feed);
    let site_url = extract_site_url(&parsed.feed);
    if !title.is_empty() || !site_url.is_empty() {
        let _ = crate::db::repo::update_feed_title_and_site(pool, feed_id, &title, &site_url).await;
    }

    let ttl = extract_recommended_interval(&parsed.feed);
    let skip_hours = extract_skip_hours_mask(&parsed.feed);
    let skip_days = extract_skip_days_mask(&parsed.feed);
    let _ = crate::db::repo::update_feed_schedule(pool, feed_id, ttl, skip_hours, skip_days).await;

    let icon_url = extract_feed_icon_url(&parsed.feed);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let _ = crate::db::repo::update_feed_last_updated(pool, feed_id, now).await;

    tracing::debug!(
        feed_id,
        entries = parsed.entries.len(),
        "processing feed entries"
    );

    let mut inserted = 0;
    for entry in &parsed.entries {
        let url = extract_entry_url(entry);
        if url.is_empty() {
            continue;
        }
        let exists = crate::db::repo::item_exists_by_url(pool, feed_id, &url)
            .await
            .unwrap_or(true);
        if exists {
            continue;
        }
        let entry_title = extract_entry_title(entry);
        let author = extract_entry_author(entry, &parsed.feed);
        let html = extract_entry_html(entry);
        let created_on_time = extract_entry_timestamp(entry);
        if crate::db::repo::insert_item(
            pool,
            feed_id,
            &entry_title,
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

    Ok((inserted, icon_url))
}

pub fn is_feed_due(
    last_updated: i64,
    interval_minutes: i64,
    feed_ttl_minutes: i64,
    consecutive_failures: i64,
    skip_hours_mask: i64,
    skip_days_mask: i64,
    now: i64,
) -> bool {
    if last_updated == 0 {
        return true;
    }

    if skip_hours_mask != 0 || skip_days_mask != 0 {
        let utc_hour = (now % 86400) / 3600;
        if skip_hours_mask & (1 << utc_hour) != 0 {
            return false;
        }
        let utc_day = (now / 86400 + 4) % 7;
        if skip_days_mask & (1 << utc_day) != 0 {
            return false;
        }
    }

    let base_minutes = interval_minutes.max(feed_ttl_minutes);
    let backoff = 2i64.pow(consecutive_failures.min(6) as u32);
    let effective_interval = base_minutes * 60 * backoff;
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
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, FEED_ACCEPT)
        .send()
        .await?
        .error_for_status()?;
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

pub async fn fetch_favicon_from_url(
    client: &reqwest::Client,
    pool: &SqlitePool,
    feed_id: i64,
    favicon_url: &str,
    existing_favicon_id: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

    let mut req = client.get(favicon_url);
    if !stored_etag.is_empty() {
        req = req.header("If-None-Match", &stored_etag);
    }

    let icon_resp = req.send().await?.error_for_status()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if icon_resp.status() == reqwest::StatusCode::NOT_MODIFIED {
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

pub async fn fetch_favicon_from_html(
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
    fetch_favicon_from_url(client, pool, feed_id, &favicon_url, existing_favicon_id).await
}

const SCHEDULER_TICK: Duration = Duration::from_secs(60);
const CONCURRENT_FETCHES: usize = 3;

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
                f.feed_ttl_minutes,
                f.consecutive_failures,
                f.skip_hours_mask,
                f.skip_days_mask,
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

    tracing::debug!(
        feed_id = feed.id,
        consecutive_failures = feed.consecutive_failures,
        "starting feed fetch"
    );

    let result = match fetch_feed(client, &feed.url).await {
        Ok(body) => match process_feed(pool, feed.id, &body).await {
            Ok(r) => {
                tracing::info!(feed_id = feed.id, new_items = r.0, "feed processed");
                Some(r)
            }
            Err(e) => {
                tracing::warn!(feed_id = feed.id, error = %e, "failed to process feed");
                None
            }
        },
        Err(e) => {
            tracing::warn!(feed_id = feed.id, error = %e, "failed to fetch feed");
            None
        }
    };

    if result.is_some() {
        let _ = crate::db::repo::reset_failures(pool, feed.id).await;
    } else {
        let _ = crate::db::repo::increment_failures(pool, feed.id).await;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if should_refresh_favicon(feed.favicon_id, feed.favicon_last_checked, now) {
        let feed_icon_url = result.as_ref().and_then(|(_, icon)| icon.as_deref());

        let icon_result = if let Some(icon_url) = feed_icon_url {
            fetch_favicon_from_url(client, pool, feed.id, icon_url, feed.favicon_id).await
        } else if !feed.site_url.is_empty() {
            fetch_favicon_from_html(client, pool, feed.id, &feed.site_url, feed.favicon_id).await
        } else {
            Ok(())
        };

        if let Err(e) = icon_result {
            tracing::warn!(feed_id = feed.id, error = %e, "failed to fetch favicon");
        }
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
            tracing::debug!("no feeds due for refresh");
            continue;
        }

        tracing::info!(count = due.len(), "feeds due for refresh");

        let mut set = tokio::task::JoinSet::new();
        for feed in due {
            if set.len() >= CONCURRENT_FETCHES {
                set.join_next().await;
            }
            let client = client.clone();
            let pool = pool.clone();
            set.spawn(async move {
                fetch_one_feed(&client, &pool, &feed).await;
            });
        }
        while set.join_next().await.is_some() {}
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

    fn sample_rss_with_ttl() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>TTL Blog</title>
    <link>https://ttl.example.com</link>
    <ttl>30</ttl>
    <item>
      <title>Post</title>
      <link>https://ttl.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_syndication() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:sy="http://purl.org/rss/1.0/modules/syndication/">
  <channel>
    <title>Syndication Blog</title>
    <link>https://syn.example.com</link>
    <sy:updatePeriod>daily</sy:updatePeriod>
    <sy:updateFrequency>2</sy:updateFrequency>
    <item>
      <title>Post</title>
      <link>https://syn.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_ttl_and_syndication() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:sy="http://purl.org/rss/1.0/modules/syndication/">
  <channel>
    <title>Both Blog</title>
    <link>https://both.example.com</link>
    <ttl>45</ttl>
    <sy:updatePeriod>hourly</sy:updatePeriod>
    <sy:updateFrequency>1</sy:updateFrequency>
    <item>
      <title>Post</title>
      <link>https://both.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_skip_hours() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Skip Hours Blog</title>
    <link>https://skip.example.com</link>
    <skipHours>
      <hour>0</hour>
      <hour>1</hour>
      <hour>2</hour>
      <hour>3</hour>
    </skipHours>
    <item>
      <title>Post</title>
      <link>https://skip.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_skip_days() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Skip Days Blog</title>
    <link>https://skip.example.com</link>
    <skipDays>
      <day>Saturday</day>
      <day>Sunday</day>
    </skipDays>
    <item>
      <title>Post</title>
      <link>https://skip.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_dc_creator() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>DC Blog</title>
    <link>https://dc.example.com</link>
    <item>
      <title>DC Post</title>
      <link>https://dc.example.com/1</link>
      <dc:creator>Dublin Core Author</dc:creator>
    </item>
  </channel>
</rss>"#
    }

    fn sample_rss_with_image() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Image Blog</title>
    <link>https://img.example.com</link>
    <image>
      <url>https://img.example.com/logo.png</url>
      <title>Logo</title>
      <link>https://img.example.com</link>
    </image>
    <item>
      <title>Post</title>
      <link>https://img.example.com/1</link>
    </item>
  </channel>
</rss>"#
    }

    fn sample_atom_with_icon() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Icon Feed</title>
  <link href="https://icon.example.com" rel="alternate"/>
  <icon>https://icon.example.com/favicon.png</icon>
  <entry>
    <title>Post</title>
    <link href="https://icon.example.com/1"/>
    <id>1</id>
  </entry>
</feed>"#
    }

    fn sample_json_feed() -> &'static [u8] {
        br#"{
  "version": "https://jsonfeed.org/version/1.1",
  "title": "JSON Blog",
  "home_page_url": "https://json.example.com",
  "items": [
    {
      "id": "1",
      "title": "JSON Post",
      "url": "https://json.example.com/1",
      "content_html": "<p>JSON content</p>",
      "authors": [{"name": "Charlie"}],
      "date_published": "2025-01-01T00:00:00Z"
    }
  ]
}"#
    }

    fn sample_rss_with_content_encoded() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Content Blog</title>
    <link>https://content.example.com</link>
    <item>
      <title>Rich Post</title>
      <link>https://content.example.com/1</link>
      <description>Summary only</description>
      <content:encoded><![CDATA[<p>Full rich content</p>]]></content:encoded>
    </item>
  </channel>
</rss>"#
    }

    #[test]
    fn entry_extraction_from_rss() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        let e = &parsed.entries[0];
        assert_eq!(extract_entry_title(e), "First Post");
        assert_eq!(extract_entry_url(e), "https://example.com/first");
        assert_eq!(extract_entry_html(e), "Hello world");
        assert!(extract_entry_timestamp(e) > 0);
    }

    #[test]
    fn entry_extraction_from_atom() {
        let parsed = feedparser_rs::parse(sample_atom()).unwrap();
        let e = &parsed.entries[0];
        assert_eq!(extract_entry_title(e), "Entry One");
        assert_eq!(extract_entry_author(e, &parsed.feed), "Bob");
        assert!(extract_entry_html(e).contains("Rich content"));
        assert_eq!(extract_entry_url(e), "https://atom.example.com/one");
    }

    #[test]
    fn entry_extraction_from_json_feed() {
        let parsed = feedparser_rs::parse(sample_json_feed()).unwrap();
        let e = &parsed.entries[0];
        assert_eq!(extract_entry_title(e), "JSON Post");
        assert_eq!(extract_entry_author(e, &parsed.feed), "Charlie");
        assert!(extract_entry_html(e).contains("JSON content"));
        assert_eq!(extract_entry_url(e), "https://json.example.com/1");
        assert!(extract_entry_timestamp(e) > 0);
    }

    #[test]
    fn entry_extraction_bare_minimum() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>T</title>
<item><guid isPermaLink="false">urn:bare</guid></item>
</channel></rss>"#;
        let parsed = feedparser_rs::parse(xml as &[u8]).unwrap();
        assert!(!parsed.entries.is_empty());
        let e = &parsed.entries[0];
        assert_eq!(extract_entry_title(e), "");
        assert_eq!(extract_entry_author(e, &parsed.feed), "");
        assert_eq!(extract_entry_html(e), "");
        assert_eq!(extract_entry_timestamp(e), 0);
    }

    #[test]
    fn entry_uses_summary_when_no_content() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>T</title><link>https://x.com</link>
<item><title>T</title><link>https://x.com/1</link><description>Summary only</description></item>
</channel></rss>"#;
        let parsed = feedparser_rs::parse(xml as &[u8]).unwrap();
        assert_eq!(extract_entry_html(&parsed.entries[0]), "Summary only");
    }

    #[test]
    fn entry_prefers_content_over_summary() {
        let parsed = feedparser_rs::parse(sample_rss_with_content_encoded()).unwrap();
        let html = extract_entry_html(&parsed.entries[0]);
        assert!(html.contains("Full rich content"));
        assert!(!html.contains("Summary only"));
    }

    #[test]
    fn entry_uses_updated_when_no_published() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>T</title><link href="https://x.com" rel="alternate"/>
  <entry>
    <title>T</title><link href="https://x.com/1"/>
    <updated>2025-06-15T12:00:00Z</updated>
  </entry>
</feed>"#;
        let parsed = feedparser_rs::parse(xml as &[u8]).unwrap();
        assert!(extract_entry_timestamp(&parsed.entries[0]) > 0);
    }

    #[test]
    fn entry_uses_dc_creator_fallback() {
        let parsed = feedparser_rs::parse(sample_rss_with_dc_creator()).unwrap();
        assert_eq!(
            extract_entry_author(&parsed.entries[0], &parsed.feed),
            "Dublin Core Author"
        );
    }

    #[test]
    fn extract_site_url_from_rss() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        let site = extract_site_url(&parsed.feed);
        assert_eq!(site, "https://example.com");
    }

    #[test]
    fn extract_site_url_from_atom() {
        let parsed = feedparser_rs::parse(sample_atom()).unwrap();
        let site = extract_site_url(&parsed.feed);
        assert_eq!(site, "https://atom.example.com");
    }

    #[test]
    fn extract_feed_title_from_rss() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        assert_eq!(extract_feed_title(&parsed.feed), "Example Blog");
    }

    #[test]
    fn extract_recommended_interval_from_ttl() {
        let parsed = feedparser_rs::parse(sample_rss_with_ttl()).unwrap();
        assert_eq!(extract_recommended_interval(&parsed.feed), 30);
    }

    #[test]
    fn extract_recommended_interval_from_syndication() {
        let parsed = feedparser_rs::parse(sample_rss_with_syndication()).unwrap();
        assert_eq!(extract_recommended_interval(&parsed.feed), 720);
    }

    #[test]
    fn extract_recommended_interval_ttl_precedence() {
        let parsed = feedparser_rs::parse(sample_rss_with_ttl_and_syndication()).unwrap();
        assert_eq!(extract_recommended_interval(&parsed.feed), 45);
    }

    #[test]
    fn extract_recommended_interval_no_hints() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        assert_eq!(extract_recommended_interval(&parsed.feed), 0);
    }

    #[test]
    fn extract_skip_hours_mask_parses_hours() {
        let parsed = feedparser_rs::parse(sample_rss_with_skip_hours()).unwrap();
        let mask = extract_skip_hours_mask(&parsed.feed);
        assert_eq!(mask, 0b1111);
        assert_ne!(mask & (1 << 0), 0);
        assert_ne!(mask & (1 << 1), 0);
        assert_ne!(mask & (1 << 2), 0);
        assert_ne!(mask & (1 << 3), 0);
        assert_eq!(mask & (1 << 4), 0);
    }

    #[test]
    fn extract_skip_hours_mask_empty() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        assert_eq!(extract_skip_hours_mask(&parsed.feed), 0);
    }

    #[test]
    fn extract_skip_days_mask_parses_days() {
        let parsed = feedparser_rs::parse(sample_rss_with_skip_days()).unwrap();
        let mask = extract_skip_days_mask(&parsed.feed);
        assert_ne!(mask & (1 << 0), 0);
        assert_ne!(mask & (1 << 6), 0);
        assert_eq!(mask & (1 << 1), 0);
    }

    #[test]
    fn extract_skip_days_mask_empty() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        assert_eq!(extract_skip_days_mask(&parsed.feed), 0);
    }

    #[test]
    fn extract_feed_icon_url_from_atom_icon() {
        let parsed = feedparser_rs::parse(sample_atom_with_icon()).unwrap();
        let icon = extract_feed_icon_url(&parsed.feed);
        assert_eq!(
            icon.as_deref(),
            Some("https://icon.example.com/favicon.png")
        );
    }

    #[test]
    fn extract_feed_icon_url_none() {
        let parsed = feedparser_rs::parse(sample_rss()).unwrap();
        assert!(extract_feed_icon_url(&parsed.feed).is_none());
    }

    #[test]
    fn extract_feed_icon_url_from_rss_image() {
        let parsed = feedparser_rs::parse(sample_rss_with_image()).unwrap();
        let icon = extract_feed_icon_url(&parsed.feed);
        assert_eq!(icon.as_deref(), Some("https://img.example.com/logo.png"));
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

        let (inserted, _) = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(inserted, 2);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.title, "Example Blog");
        assert_eq!(feed.site_url, "https://example.com");
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

        let (first, _) = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(first, 2);

        let (second, _) = process_feed(&pool, feed.id, sample_rss()).await.unwrap();
        assert_eq!(second, 0);
    }

    #[test]
    fn is_feed_due_never_fetched() {
        assert!(is_feed_due(0, 60, 0, 0, 0, 0, 1000));
    }

    #[test]
    fn is_feed_due_recently_fetched() {
        let now = 1_700_000_000;
        let last = now - 1800;
        assert!(!is_feed_due(last, 60, 0, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_interval_elapsed() {
        let now = 1_700_000_000;
        let last = now - 3601;
        assert!(is_feed_due(last, 60, 0, 0, 0, 0, now));
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

        let (inserted, _) = process_feed(&pool, feed.id, sample_atom()).await.unwrap();
        assert_eq!(inserted, 1);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.title, "Atom Feed");
        assert_eq!(feed.site_url, "https://atom.example.com");
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
        match result {
            Err(_) => {}
            Ok((count, _)) => assert_eq!(count, 0),
        }
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
        match result {
            Err(_) => {}
            Ok((count, _)) => assert_eq!(count, 0),
        }
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
        match result {
            Err(_) => {}
            Ok((count, _)) => assert_eq!(count, 0),
        }
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
        match result {
            Err(_) => {}
            Ok((count, _)) => assert_eq!(count, 0),
        }
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
        let (inserted, _) = process_feed(&pool, feed.id, xml).await.unwrap();
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
        let (inserted, _) = process_feed(&pool, feed.id, xml).await.unwrap();
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
        let (inserted, _) = process_feed(&pool, feed.id, xml).await.unwrap();
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
        let (inserted, _) = process_feed(&pool, feed.id, xml).await.unwrap();
        assert_eq!(inserted, 2);
    }

    #[test]
    fn is_feed_due_future_last_updated() {
        let now = 1_700_000_000;
        let last = now + 3600;
        assert!(!is_feed_due(last, 60, 0, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_zero_interval() {
        let now = 1_700_000_000;
        let last = now - 1;
        assert!(is_feed_due(last, 0, 0, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_exact_boundary() {
        let now = 1_700_000_000;
        let last = now - 3600;
        assert!(is_feed_due(last, 60, 0, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_respects_feed_ttl() {
        let now = 1_700_000_000;
        let last = now - 1500;
        assert!(!is_feed_due(last, 15, 30, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_user_interval_overrides_short_ttl() {
        let now = 1_700_000_000;
        let last = now - 1900;
        assert!(!is_feed_due(last, 60, 15, 0, 0, 0, now));
    }

    #[test]
    fn is_feed_due_skip_hours_blocks() {
        let now = 3 * 3600;
        let skip_hours_mask = 1i64 << 3;
        assert!(!is_feed_due(1, 0, 0, 0, skip_hours_mask, 0, now));
    }

    #[test]
    fn is_feed_due_skip_days_blocks() {
        let now = 4 * 86400;
        let skip_days_mask = 1i64 << 1;
        assert!(!is_feed_due(1, 0, 0, 0, 0, skip_days_mask, now));
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

    #[test]
    fn fetch_feed_accept_header_is_set() {
        assert!(!FEED_ACCEPT.is_empty());
        assert!(FEED_ACCEPT.contains("application/rss+xml"));
        assert!(FEED_ACCEPT.contains("application/atom+xml"));
    }

    #[test]
    fn user_agent_is_set() {
        assert!(USER_AGENT.starts_with("feedme/"));
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
        assert!(is_feed_due(last, 60, 0, 1, 0, 0, now));
    }

    #[test]
    fn is_feed_due_with_backoff_delays_retry() {
        let now = 1_700_000_000;
        let last = now - 3700;
        assert!(!is_feed_due(last, 60, 0, 1, 0, 0, now));
    }

    #[test]
    fn is_feed_due_backoff_capped() {
        let now = 1_700_000_000;
        let last = now - (60 * 64 * 60 + 1);
        assert!(is_feed_due(last, 60, 0, 10, 0, 0, now));

        let last_too_recent = now - (60 * 64 * 60 - 1);
        assert!(!is_feed_due(last_too_recent, 60, 0, 10, 0, 0, now));
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
    async fn process_feed_stores_ttl() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://ttl.example.com/feed", 60)
            .await
            .unwrap();

        let (inserted, _) = process_feed(&pool, feed.id, sample_rss_with_ttl())
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.feed_ttl_minutes, 30);
    }

    #[tokio::test]
    async fn process_feed_stores_skip_hours() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://skip.example.com/feed", 60)
            .await
            .unwrap();

        process_feed(&pool, feed.id, sample_rss_with_skip_hours())
            .await
            .unwrap();

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.skip_hours_mask, 0b1111);
    }

    #[tokio::test]
    async fn process_feed_stores_skip_days() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://skip.example.com/feed", 60)
            .await
            .unwrap();

        process_feed(&pool, feed.id, sample_rss_with_skip_days())
            .await
            .unwrap();

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(feed.skip_days_mask, 0);
    }

    #[tokio::test]
    async fn process_feed_json_feed() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://json.example.com/feed", 60)
            .await
            .unwrap();

        let (inserted, _) = process_feed(&pool, feed.id, sample_json_feed())
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let feed = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(feed.title, "JSON Blog");
    }

    #[tokio::test]
    async fn process_feed_returns_feed_icon_url() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://icon.example.com/feed", 60)
            .await
            .unwrap();

        let (_, icon_url) = process_feed(&pool, feed.id, sample_atom_with_icon())
            .await
            .unwrap();
        assert_eq!(
            icon_url.as_deref(),
            Some("https://icon.example.com/favicon.png")
        );
    }

    #[tokio::test]
    async fn process_feed_content_encoded() {
        let pool = test_pool().await;
        let feed = crate::db::repo::insert_feed(&pool, "https://content.example.com/feed", 60)
            .await
            .unwrap();

        let (inserted, _) = process_feed(&pool, feed.id, sample_rss_with_content_encoded())
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let items = crate::db::repo::get_items_since(&pool, 0, 50)
            .await
            .unwrap();
        assert!(items[0].html.contains("Full rich content"));
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

    #[test]
    fn concurrent_fetches_is_positive() {
        assert!(CONCURRENT_FETCHES > 0);
        assert_eq!(CONCURRENT_FETCHES, 3);
    }
}
