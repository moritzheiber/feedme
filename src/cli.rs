use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "feedme", about = "Fever API compatible RSS feed aggregator")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    Feed {
        #[command(subcommand)]
        action: FeedAction,
    },
}

#[derive(Subcommand)]
pub enum FeedAction {
    Add {
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "60")]
        interval: i64,
    },
    List,
    Update {
        id: i64,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        interval: Option<i64>,
    },
    Remove {
        id: i64,
    },
    Import {
        file: String,
    },
    Export {
        file: String,
    },
}

pub async fn handle_feed_action(
    pool: &sqlx::SqlitePool,
    action: FeedAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FeedAction::Add { url, interval } => {
            let feed = crate::db::repo::insert_feed(pool, &url, interval).await?;
            tracing::info!(id = feed.id, url = %feed.url, "feed added");
        }
        FeedAction::List => {
            let feeds = crate::db::repo::list_feeds(pool).await?;
            for feed in feeds {
                tracing::info!(
                    id = feed.id,
                    url = %feed.url,
                    interval_minutes = feed.fetch_interval_minutes,
                    "feed"
                );
            }
        }
        FeedAction::Update { id, url, interval } => {
            let updated = crate::db::repo::update_feed(pool, id, url.as_deref(), interval).await?;
            if updated {
                tracing::info!(id, "feed updated");
            } else {
                tracing::warn!(id, "feed not found");
            }
        }
        FeedAction::Remove { id } => {
            let deleted = crate::db::repo::delete_feed(pool, id).await?;
            if deleted {
                tracing::info!(id, "feed removed");
            } else {
                tracing::warn!(id, "feed not found");
            }
        }
        FeedAction::Import { file } => {
            let content = std::fs::read_to_string(&file)?;
            let feeds = extract_feeds_from_opml(&content)?;
            let mut imported = 0;
            for (url, _title) in feeds {
                if !crate::db::repo::feed_exists_by_url(pool, &url).await? {
                    crate::db::repo::insert_feed(pool, &url, 60).await?;
                    imported += 1;
                }
            }
            tracing::info!(count = imported, "feeds imported");
        }
        FeedAction::Export { file } => {
            let feeds = crate::db::repo::list_feeds(pool).await?;
            let xml = feeds_to_opml(&feeds)?;
            std::fs::write(&file, xml)?;
            tracing::info!(path = %file, count = feeds.len(), "feeds exported");
        }
    }
    Ok(())
}

pub fn extract_feeds_from_opml(content: &str) -> Result<Vec<(String, String)>, opml::Error> {
    let doc = opml::OPML::from_str(content)?;
    let mut feeds = Vec::new();
    collect_outlines(&doc.body.outlines, &mut feeds);
    Ok(feeds)
}

fn collect_outlines(outlines: &[opml::Outline], feeds: &mut Vec<(String, String)>) {
    for outline in outlines {
        if let Some(xml_url) = &outline.xml_url {
            feeds.push((xml_url.clone(), outline.text.clone()));
        }
        collect_outlines(&outline.outlines, feeds);
    }
}

pub fn feeds_to_opml(feeds: &[crate::db::models::Feed]) -> Result<String, opml::Error> {
    if feeds.is_empty() {
        return Ok(String::new());
    }
    let mut doc = opml::OPML::default();
    for feed in feeds {
        doc.add_feed(&feed.title, &feed.url);
    }
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_serve() {
        let cli = Cli::parse_from(["feedme", "serve"]);
        assert!(matches!(
            cli.command,
            Command::Serve {
                host: None,
                port: None
            }
        ));
    }

    #[test]
    fn parse_serve_with_overrides() {
        let cli = Cli::parse_from(["feedme", "serve", "--host", "127.0.0.1", "--port", "3000"]);
        match cli.command {
            Command::Serve { host, port } => {
                assert_eq!(host.unwrap(), "127.0.0.1");
                assert_eq!(port.unwrap(), 3000);
            }
            _ => panic!("expected Serve"),
        }
    }

    #[test]
    fn parse_feed_add() {
        let cli = Cli::parse_from(["feedme", "feed", "add", "--url", "https://example.com/feed"]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Add { url, interval },
            } => {
                assert_eq!(url, "https://example.com/feed");
                assert_eq!(interval, 60);
            }
            _ => panic!("expected Feed Add"),
        }
    }

    #[test]
    fn parse_feed_add_with_interval() {
        let cli = Cli::parse_from([
            "feedme",
            "feed",
            "add",
            "--url",
            "https://example.com/feed",
            "--interval",
            "30",
        ]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Add { url, interval },
            } => {
                assert_eq!(url, "https://example.com/feed");
                assert_eq!(interval, 30);
            }
            _ => panic!("expected Feed Add"),
        }
    }

    #[test]
    fn parse_feed_list() {
        let cli = Cli::parse_from(["feedme", "feed", "list"]);
        assert!(matches!(
            cli.command,
            Command::Feed {
                action: FeedAction::List
            }
        ));
    }

    #[test]
    fn parse_feed_update() {
        let cli = Cli::parse_from([
            "feedme",
            "feed",
            "update",
            "42",
            "--url",
            "https://new.com/feed",
            "--interval",
            "120",
        ]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Update { id, url, interval },
            } => {
                assert_eq!(id, 42);
                assert_eq!(url.unwrap(), "https://new.com/feed");
                assert_eq!(interval.unwrap(), 120);
            }
            _ => panic!("expected Feed Update"),
        }
    }

    #[test]
    fn parse_feed_update_partial() {
        let cli = Cli::parse_from(["feedme", "feed", "update", "5", "--interval", "15"]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Update { id, url, interval },
            } => {
                assert_eq!(id, 5);
                assert!(url.is_none());
                assert_eq!(interval.unwrap(), 15);
            }
            _ => panic!("expected Feed Update"),
        }
    }

    #[test]
    fn parse_feed_remove() {
        let cli = Cli::parse_from(["feedme", "feed", "remove", "7"]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Remove { id },
            } => {
                assert_eq!(id, 7);
            }
            _ => panic!("expected Feed Remove"),
        }
    }

    #[test]
    fn parse_feed_import() {
        let cli = Cli::parse_from(["feedme", "feed", "import", "feeds.opml"]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Import { file },
            } => {
                assert_eq!(file, "feeds.opml");
            }
            _ => panic!("expected Feed Import"),
        }
    }

    #[test]
    fn extract_feeds_from_valid_opml() {
        let opml_content = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head><title>Feeds</title></head>
  <body>
    <outline text="Tech" title="Tech">
      <outline type="rss" text="Blog A" title="Blog A" xmlUrl="https://a.com/feed" htmlUrl="https://a.com"/>
      <outline type="rss" text="Blog B" title="Blog B" xmlUrl="https://b.com/rss" htmlUrl="https://b.com"/>
    </outline>
    <outline type="rss" text="Blog C" title="Blog C" xmlUrl="https://c.com/atom" htmlUrl="https://c.com"/>
  </body>
</opml>"#;

        let feeds = extract_feeds_from_opml(opml_content).unwrap();
        assert_eq!(feeds.len(), 3);
        assert_eq!(
            feeds[0],
            ("https://a.com/feed".to_string(), "Blog A".to_string())
        );
        assert_eq!(
            feeds[1],
            ("https://b.com/rss".to_string(), "Blog B".to_string())
        );
        assert_eq!(
            feeds[2],
            ("https://c.com/atom".to_string(), "Blog C".to_string())
        );
    }

    #[test]
    fn extract_feeds_skips_outlines_without_xml_url() {
        let opml_content = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head><title>Feeds</title></head>
  <body>
    <outline text="Folder" title="Folder"/>
    <outline type="rss" text="Real Feed" xmlUrl="https://real.com/feed"/>
  </body>
</opml>"#;

        let feeds = extract_feeds_from_opml(opml_content).unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].0, "https://real.com/feed");
    }

    #[tokio::test]
    async fn handle_feed_add_and_list() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        handle_feed_action(
            &pool,
            FeedAction::Add {
                url: "https://example.com/feed".to_string(),
                interval: 60,
            },
        )
        .await
        .unwrap();

        let feeds = crate::db::repo::list_feeds(&pool).await.unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].url, "https://example.com/feed");
    }

    #[tokio::test]
    async fn handle_feed_update_changes_interval() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        handle_feed_action(
            &pool,
            FeedAction::Update {
                id: feed.id,
                url: None,
                interval: Some(30),
            },
        )
        .await
        .unwrap();

        let updated = crate::db::repo::get_feed(&pool, feed.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.fetch_interval_minutes, 30);
    }

    #[tokio::test]
    async fn handle_feed_remove_deletes_feed() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();

        handle_feed_action(&pool, FeedAction::Remove { id: feed.id })
            .await
            .unwrap();

        let feeds = crate::db::repo::list_feeds(&pool).await.unwrap();
        assert!(feeds.is_empty());
    }

    #[tokio::test]
    async fn handle_feed_import_from_opml() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let opml_path = "/tmp/feedme_test_import.opml";
        std::fs::write(
            opml_path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <body>
    <outline type="rss" text="Feed A" xmlUrl="https://a.com/feed"/>
    <outline type="rss" text="Feed B" xmlUrl="https://b.com/feed"/>
  </body>
</opml>"#,
        )
        .unwrap();

        handle_feed_action(
            &pool,
            FeedAction::Import {
                file: opml_path.to_string(),
            },
        )
        .await
        .unwrap();

        let feeds = crate::db::repo::list_feeds(&pool).await.unwrap();
        assert_eq!(feeds.len(), 2);

        std::fs::remove_file(opml_path).unwrap();
    }

    #[tokio::test]
    async fn handle_feed_import_skips_duplicates() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        crate::db::repo::insert_feed(&pool, "https://a.com/feed", 60)
            .await
            .unwrap();

        let opml_path = "/tmp/feedme_test_import_dup.opml";
        std::fs::write(
            opml_path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <body>
    <outline type="rss" text="Feed A" xmlUrl="https://a.com/feed"/>
    <outline type="rss" text="Feed C" xmlUrl="https://c.com/feed"/>
  </body>
</opml>"#,
        )
        .unwrap();

        handle_feed_action(
            &pool,
            FeedAction::Import {
                file: opml_path.to_string(),
            },
        )
        .await
        .unwrap();

        let feeds = crate::db::repo::list_feeds(&pool).await.unwrap();
        assert_eq!(feeds.len(), 2);

        std::fs::remove_file(opml_path).unwrap();
    }

    #[test]
    fn parse_feed_export() {
        let cli = Cli::parse_from(["feedme", "feed", "export", "feeds.opml"]);
        match cli.command {
            Command::Feed {
                action: FeedAction::Export { file },
            } => {
                assert_eq!(file, "feeds.opml");
            }
            _ => panic!("expected Feed Export"),
        }
    }

    #[test]
    fn feeds_to_opml_produces_valid_xml() {
        let feeds = vec![
            crate::db::models::Feed {
                id: 1,
                title: "Blog A".to_string(),
                url: "https://a.com/feed".to_string(),
                site_url: "https://a.com".to_string(),
                favicon_id: 0,
                is_spark: 0,
                last_updated_on_time: 0,
                fetch_interval_minutes: 60,
                consecutive_failures: 0,
                favicon_last_checked: 0,
            },
            crate::db::models::Feed {
                id: 2,
                title: "Blog B".to_string(),
                url: "https://b.com/rss".to_string(),
                site_url: "https://b.com".to_string(),
                favicon_id: 0,
                is_spark: 0,
                last_updated_on_time: 0,
                fetch_interval_minutes: 30,
                consecutive_failures: 0,
                favicon_last_checked: 0,
            },
        ];

        let xml = feeds_to_opml(&feeds).unwrap();
        let parsed = opml::OPML::from_str(&xml).unwrap();
        assert_eq!(parsed.body.outlines.len(), 2);
        assert_eq!(
            parsed.body.outlines[0].xml_url.as_deref(),
            Some("https://a.com/feed")
        );
        assert_eq!(parsed.body.outlines[0].text, "Blog A");
        assert_eq!(
            parsed.body.outlines[1].xml_url.as_deref(),
            Some("https://b.com/rss")
        );
    }

    #[test]
    fn feeds_to_opml_empty_list() {
        let xml = feeds_to_opml(&[]).unwrap();
        assert!(xml.is_empty());
    }

    #[tokio::test]
    async fn handle_feed_export_writes_file() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let feed = crate::db::repo::insert_feed(&pool, "https://example.com/feed", 60)
            .await
            .unwrap();
        crate::db::repo::update_feed_title_and_site(
            &pool,
            feed.id,
            "Example",
            "https://example.com",
        )
        .await
        .unwrap();

        let export_path = "/tmp/feedme_test_export.opml";
        handle_feed_action(
            &pool,
            FeedAction::Export {
                file: export_path.to_string(),
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(export_path).unwrap();
        let parsed = opml::OPML::from_str(&content).unwrap();
        assert_eq!(parsed.body.outlines.len(), 1);
        assert_eq!(
            parsed.body.outlines[0].xml_url.as_deref(),
            Some("https://example.com/feed")
        );

        std::fs::remove_file(export_path).unwrap();
    }
}
