//! Where scraped articles land on disk.
//!
//!   <out>/<site>/articles.jsonl      (default)  one JSON object per line
//!   <out>/<site>/articles/<file>.json (--format files)
//!   <out>/<site>/_seen_urls.txt      URLs already saved, used by --resume
//!   <out>/<site>/summary.json        per-site report

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jsonl,
    Files,
}

#[derive(Debug, Clone, Serialize)]
pub struct Article {
    pub site: String,
    pub url: String,
    /// Feed page this link was found on.
    pub source_page: String,
    pub http_status: u16,
    pub fetched_at: String,
    pub title: Option<String>,
    pub author: Option<String>,
    /// Normalised to RFC 3339 when the raw value could be parsed.
    pub publish_date: Option<String>,
    pub publish_date_raw: Option<String>,
    pub description: Option<String>,
    pub word_count: usize,
    pub images: Vec<String>,
    pub comments: Vec<String>,
    pub internal_links: Vec<String>,
    /// Which fields came from the configured selector and which from a fallback.
    pub field_sources: BTreeMap<String, String>,

    #[serde(skip)]
    pub publish_dt: Option<DateTime<Utc>>,
}

pub struct SiteWriter {
    dir: PathBuf,
    format: Format,
    jsonl: Option<BufWriter<File>>,
    seen: BufWriter<File>,
}

impl SiteWriter {
    /// Opens (or reopens, with `resume`) the output for one site and returns the
    /// set of URLs already on disk so they are not fetched again.
    pub fn new(
        root: &Path,
        site: &str,
        format: Format,
        resume: bool,
    ) -> Result<(Self, HashSet<String>)> {
        let dir = root.join(sanitize(site));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;

        let seen_path = dir.join("_seen_urls.txt");
        let mut seen = HashSet::new();
        if resume && seen_path.exists() {
            let text = std::fs::read_to_string(&seen_path).unwrap_or_default();
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    seen.insert(line.to_string());
                }
            }
        }

        let seen_file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume)
            .truncate(!resume)
            .open(&seen_path)
            .with_context(|| format!("cannot open {}", seen_path.display()))?;

        let jsonl = if format == Format::Jsonl {
            let path = dir.join("articles.jsonl");
            Some(BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(resume)
                    .truncate(!resume)
                    .open(&path)
                    .with_context(|| format!("cannot open {}", path.display()))?,
            ))
        } else {
            std::fs::create_dir_all(dir.join("articles"))?;
            None
        };

        Ok((
            Self {
                dir,
                format,
                jsonl,
                seen: BufWriter::new(seen_file),
            },
            seen,
        ))
    }

    pub fn write(&mut self, article: &Article) -> Result<()> {
        match self.format {
            Format::Jsonl => {
                if let Some(w) = self.jsonl.as_mut() {
                    writeln!(w, "{}", serde_json::to_string(article)?)?;
                }
            }
            Format::Files => {
                let day = article
                    .publish_date
                    .as_deref()
                    .and_then(|d| d.get(..10))
                    .map(|d| d.replace('-', ""))
                    .unwrap_or_else(|| "nodate".to_string());
                let name = format!("{day}_{}.json", short_hash(&article.url));
                let path = self.dir.join("articles").join(name);
                std::fs::write(&path, serde_json::to_vec_pretty(article)?)
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }
        }
        writeln!(self.seen, "{}", article.url)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(w) = self.jsonl.as_mut() {
            w.flush()?;
        }
        self.seen.flush()?;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

pub fn short_hash(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

pub fn sanitize(name: &str) -> String {
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
