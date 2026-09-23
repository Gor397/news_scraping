//! Postgres backend, selected with `--db postgresql://...`.
//!
//! Site configs are read from a `site_configs` table (uploaded with
//! `upload_selectors.py`) and scraped articles are written to `articles` /
//! `article_images` / `article_links` instead of the `output/` directory tree.
//! `--db` and `--config-dir` can be combined to mix sources, but never
//! combined with `--out`'s article writing: one run has exactly one sink.

use crate::config::SiteConfig;
use crate::output::Article;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::Arc;
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

/// A shared Postgres connection, cheap to clone between site tasks.
#[derive(Clone)]
pub struct Db {
    client: Arc<tokio_postgres::Client>,
}

impl Db {
    /// Connect with TLS when the server offers it (`sslmode=prefer`, the
    /// libpq default), plain when it does not.
    pub async fn connect(url: &str) -> Result<Self> {
        let mut root_store = rustls::RootCertStore::empty();
        let loaded = rustls_native_certs::load_native_certs();
        for err in &loaded.errors {
            tracing::warn!("native certificate problem: {err}");
        }
        for cert in loaded.certs {
            root_store
                .add(cert)
                .map_err(|e| anyhow!("cannot load system root certificates: {e}"))?;
        }
        let connector = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let tls = MakeRustlsConnect::new(connector);

        // `connect` is happy to be given either the TLS or the NoTls maker
        // only through generics, so try TLS first and fall back on the
        // specific "server refused TLS" errors.
        let client = match tokio_postgres::connect(url, tls).await {
            Ok((client, task)) => {
                tokio::spawn(task);
                client
            }
            Err(e) => {
                let msg = e.to_string().to_ascii_lowercase();
                let tls_refused = msg.contains("ssl")
                    || msg.contains("tls")
                    || msg.contains("encryption")
                    || msg.contains("server refused");
                if !tls_refused {
                    return Err(anyhow!("postgres connection failed: {e}"));
                }
                let (client, task) = tokio_postgres::connect(url, NoTls)
                    .await
                    .map_err(|e| anyhow!("postgres connection failed (TLS and plain): {e}"))?;
                tokio::spawn(task);
                client
            }
        };
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Create the tables if they are not there yet.
    pub async fn ensure_schema(&self) -> Result<()> {
        self.client
            .batch_execute(SCHEMA)
            .await
            .context("cannot create schema in postgres")?;
        Ok(())
    }

    /// Load every config row as a `SiteConfig`, matching what
    /// `config::load_dir` produces from JSON files.
    pub async fn load_configs(&self) -> Result<Vec<SiteConfig>> {
        let rows = self
            .client
            .query(
                "SELECT host, config FROM site_configs ORDER BY host",
                &[],
            )
            .await
            .context("cannot read site_configs from postgres")?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let host: String = row.get(0);
            // The column is JSONB and `with-serde_json-1` gives `serde_json::Value`
            // a direct JSONB impl, but a binary built without that feature (or a
            // cast-to-TEXT row shape left over from older builds) yields text or
            // raw JSON bytes instead. Accept every shape so stale binaries and
            // legacy schemas both keep working.
            let raw: serde_json::Value = if row.try_get::<_, serde_json::Value>(1).is_ok() {
                row.get(1)
            } else if let Ok(text) = row.try_get::<_, String>(1) {
                serde_json::from_str(&text)
                    .with_context(|| format!("bad JSON text for '{host}'"))?
            } else {
                let bytes: Vec<u8> = row
                    .try_get(1)
                    .with_context(|| format!("bad config JSON for '{host}'"))?;
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("bad config JSON bytes for '{host}'"))?
            };
            let cfg: SiteConfig = serde_json::from_value(raw)
                .with_context(|| format!("bad config row for '{host}'"))?;
            out.push(SiteConfig {
                name: sanitize_host(&host),
                source_file: std::path::PathBuf::from(format!("db:{host}")),
                ..cfg
            });
        }
        Ok(out)
    }

    /// URLs already saved for a site, for resume across runs.
    pub async fn seen_urls(&self, site: &str) -> Result<HashSet<String>> {
        let rows = self
            .client
            .query("SELECT url FROM seen_urls WHERE site = $1", &[&site])
            .await
            .context("cannot read seen_urls from postgres")?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    /// Insert one article with its images, links and comments.
    ///
    /// Every statement is individually idempotent (upsert / ON CONFLICT DO
    /// NOTHING), so an interrupted run is healed by the next one; no explicit
    /// transaction is held because `Client::transaction` needs `&mut`.
    pub async fn save_article(&self, a: &Article) -> Result<()> {
        // TIMESTAMPTZ columns want real timestamps, not text. `fetched_at` is
        // always RFC 3339 (`Utc::now().to_rfc3339()`); `publish_date` is RFC
        // 3339 when it could be parsed, otherwise NULL.
        let fetched_at = DateTime::parse_from_rfc3339(&a.fetched_at)
            .with_context(|| format!("bad fetched_at '{}' for {}", a.fetched_at, a.url))?
            .with_timezone(&Utc);
        let publish_date = a
            .publish_date
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let row = self
            .client
            .query_one(
                "INSERT INTO articles
                    (site, url, source_page, http_status, fetched_at,
                     title, author, publish_date, publish_date_raw,
                     description, word_count, field_sources)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                 ON CONFLICT (site, url) DO UPDATE SET
                    source_page = EXCLUDED.source_page,
                    http_status = EXCLUDED.http_status,
                    fetched_at = EXCLUDED.fetched_at,
                    title = EXCLUDED.title,
                    author = EXCLUDED.author,
                    publish_date = EXCLUDED.publish_date,
                    publish_date_raw = EXCLUDED.publish_date_raw,
                    description = EXCLUDED.description,
                    word_count = EXCLUDED.word_count,
                    field_sources = EXCLUDED.field_sources
                 RETURNING id",
                &[
                    &a.site,
                    &a.url,
                    &a.source_page,
                    &(a.http_status as i32),
                    &fetched_at,
                    &a.title,
                    &a.author,
                    &publish_date,
                    &a.publish_date_raw,
                    &a.description,
                    &(a.word_count as i32),
                    &serde_json::to_value(&a.field_sources)?,
                ],
            )
            .await
            .with_context(|| format!("cannot upsert article {}", a.url))?;
        let id: i64 = row.get(0);

        self.client
            .execute(
                "DELETE FROM article_images WHERE article_id = $1",
                &[&id],
            )
            .await?;
        for img in &a.images {
            self.client
                .execute(
                    "INSERT INTO article_images (article_id, image_url) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING",
                    &[&id, img],
                )
                .await?;
        }

        for (kind, links) in [
            ("comment", a.comments.as_slice()),
            ("internal", a.internal_links.as_slice()),
        ] {
            self.client
                .execute(
                    "DELETE FROM article_links WHERE article_id = $1 AND kind = $2",
                    &[&id, &kind],
                )
                .await?;
            for link in links {
                self.client
                    .execute(
                        "INSERT INTO article_links (article_id, kind, link_url) VALUES ($1, $2, $3)
                         ON CONFLICT DO NOTHING",
                        &[&id, &kind, link],
                    )
                    .await?;
            }
        }

        self.client
            .execute(
                "INSERT INTO seen_urls (site, url) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&a.site, &a.url],
            )
            .await?;

        Ok(())
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS site_configs (
    host        TEXT PRIMARY KEY,
    config      JSONB NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS articles (
    id               BIGSERIAL PRIMARY KEY,
    site             TEXT NOT NULL,
    url              TEXT NOT NULL,
    source_page      TEXT NOT NULL,
    http_status      INTEGER,
    fetched_at       TIMESTAMPTZ,
    title            TEXT,
    author           TEXT,
    publish_date     TIMESTAMPTZ,
    publish_date_raw TEXT,
    description      TEXT,
    word_count       INTEGER,
    field_sources    JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (site, url)
);

CREATE TABLE IF NOT EXISTS article_images (
    article_id BIGINT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    image_url  TEXT NOT NULL,
    PRIMARY KEY (article_id, image_url)
);

CREATE TABLE IF NOT EXISTS article_links (
    article_id BIGINT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    link_url   TEXT NOT NULL,
    PRIMARY KEY (article_id, kind, link_url)
);

CREATE TABLE IF NOT EXISTS seen_urls (
    site       TEXT NOT NULL,
    url        TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (site, url)
);

CREATE INDEX IF NOT EXISTS articles_site_date_idx
    ON articles (site, publish_date DESC);
CREATE INDEX IF NOT EXISTS articles_fetched_at_idx
    ON articles (fetched_at DESC);
"#;

/// The Rust-side site name is a directory name when scraping to files, so keep
/// it filesystem-safe the same way `output::sanitize` does.
fn sanitize_host(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').trim_matches('.').to_string();
    if trimmed.is_empty() {
        "site".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Article;
    use std::collections::BTreeMap;
    use tokio_postgres::types::{FromSql, ToSql, Type};

    #[test]
    fn sanitizes_hosts_like_output() {
        assert_eq!(sanitize_host("en.example.com"), "en.example.com");
        assert_eq!(sanitize_host("bad host"), "bad_host");
        assert_eq!(sanitize_host("..."), "site");
    }

    #[test]
    fn jsonb_and_timestamptz_impls_are_enabled() {
        // `serde_json::Value: FromSql` needs tokio-postgres' `with-serde_json-1`
        // and `DateTime<Utc>` needs `with-chrono-0_4`; without those features
        // these impls do not exist and this test fails to compile.
        assert!(<serde_json::Value as FromSql>::accepts(&Type::JSONB));
        assert!(<DateTime<Utc> as FromSql>::accepts(&Type::TIMESTAMPTZ));
    }

    /// Compiles the `save_article` parameter list against the real `ToSql`
    /// impls, catching at build time the "cannot convert between the Rust type
    /// `&[u8]` and the Postgres type `jsonb`" class of errors.
    #[test]
    fn save_article_params_use_typed_jsonb_and_timestamps() {
        let article = Article {
            site: "example.com".into(),
            url: "https://example.com/a".into(),
            source_page: "https://example.com/feed".into(),
            http_status: 200,
            fetched_at: "2026-09-23T10:05:59.011+00:00".into(),
            title: Some("t".into()),
            author: None,
            publish_date: Some("2026-09-01T08:30:00+00:00".into()),
            publish_date_raw: Some("1 Sep 2026".into()),
            description: None,
            word_count: 12,
            images: vec![],
            comments: vec![],
            internal_links: vec![],
            field_sources: BTreeMap::from([("title".into(), "selector".into())]),
            publish_dt: None,
        };
        let fetched_at = DateTime::parse_from_rfc3339(&article.fetched_at)
            .unwrap()
            .with_timezone(&Utc);
        let publish_date = article
            .publish_date
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let _params: [&dyn ToSql; 12] = [
            &article.site,
            &article.url,
            &article.source_page,
            &(article.http_status as i32),
            &fetched_at,
            &article.title,
            &article.author,
            &publish_date,
            &article.publish_date_raw,
            &article.description,
            &(article.word_count as i32),
            &serde_json::to_value(&article.field_sources).unwrap(),
        ];
    }
}
