# feedme

A Fever API compatible RSS feed aggregator. Single-user, single binary, SQLite-backed.

## Features

- Fever API compatible -- works with any Fever API client (Reeder, Unread, etc.)
- RSS, Atom, and JSON Feed support
- Automatic feed fetching with configurable intervals and concurrent workers
- Honors feed-provided TTL, syndication update intervals, skipHours, and skipDays
- Supports `dc:creator` author fallback and `content:encoded`
- Exponential backoff for failing feeds
- Favicon extraction from feed metadata with HTML scraping fallback and ETag-based conditional refresh
- OPML import and export
- CLI for feed management

## Quick start

```sh
export FEEDME_EMAIL="you@example.com"
export FEEDME_PASSWORD="your-password"

feedme serve
```

The server starts on `0.0.0.0:8080` by default. Point your Fever API client at `http://<host>:8080/` with the API key derived from `md5(email:password)`.

## Configuration

All configuration is via environment variables. A `.env` file is supported.

| Variable | Required | Default | Description |
|---|---|---|---|
| `FEEDME_EMAIL` | yes | | User email address |
| `FEEDME_PASSWORD` | yes | | User password |
| `FEEDME_DATABASE_URL` | no | `feedme.db` | Path to SQLite database file |
| `FEEDME_HOST` | no | `0.0.0.0` | Server bind address |
| `FEEDME_PORT` | no | `8080` | Server bind port |

## CLI

```
feedme serve [--host <HOST>] [--port <PORT>]
feedme feed add --url <URL> [--interval <MINUTES>]
feedme feed list
feedme feed update <ID> [--url <URL>] [--interval <MINUTES>]
feedme feed remove <ID>
feedme feed import <FILE>
feedme feed export <FILE>
```

`serve` starts the API server and the background feed fetcher. `--host` and `--port` override the corresponding environment variables.

`feed add` adds a feed. The default fetch interval is 60 minutes.

`feed import` reads an OPML file and adds any feeds not already present. `feed export` writes all feeds to an OPML file.

## Docker

```sh
docker build -t feedme .
docker run -d \
  -e FEEDME_EMAIL="you@example.com" \
  -e FEEDME_PASSWORD="your-password" \
  -v feedme-data:/data \
  -e FEEDME_DATABASE_URL=/data/feedme.db \
  -p 8080:8080 \
  feedme
```

## Building from source

```sh
cargo build --release
```

Requires Rust 1.85+ (edition 2024).

## API

Implements the [Fever API](https://web.archive.org/web/20230616124016/https://feedafever.com/api). Single endpoint: `POST /` with query parameters.

Authentication: include `api_key` in the POST form data. The key is `md5(email:password)`.

A `GET /` discovery endpoint is available for clients that perform auto-detection (e.g. Unread).

Read endpoints (via query parameters, combinable):

- `?api` -- base authenticated request
- `?api&feeds` -- list feeds
- `?api&groups` -- list groups (always empty; groups are not supported)
- `?api&favicons` -- list favicons
- `?api&items` -- list items (supports `since_id`, `max_id`, `with_ids`)
- `?api&unread_item_ids` -- comma-separated unread item IDs
- `?api&saved_item_ids` -- comma-separated saved item IDs

All feed responses include an empty `feeds_groups` array for client compatibility.

Write endpoints (via POST form data):

- `mark=item&as=read&id=<ID>` -- mark item read
- `mark=item&as=saved&id=<ID>` -- save item
- `mark=item&as=unsaved&id=<ID>` -- unsave item
- `mark=feed&as=read&id=<ID>&before=<TIMESTAMP>` -- mark feed items read before timestamp
- `mark=group&as=read&id=<ID>&before=<TIMESTAMP>` -- mark all items read before timestamp (groups are ignored)
- `unread_recently_read=1` -- mark recently read items as unread