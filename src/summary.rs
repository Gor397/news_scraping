//! Per-site and per-run reporting.
//!
//! The point of the report is to answer "which of my 108 selector files are
//! actually working?" - so it separates *fetch* problems (site unreachable,
//! pagination dead) from *selector* problems (page fetched fine, but the
//! title_selector matched nothing).

use crate::config::SiteConfig;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const FIELDS: [&str; 7] = [
    "title",
    "description",
    "author",
    "publish_date",
    "images",
    "comments",
    "internal_links",
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct FieldStat {
    /// Was a selector provided for this field at all?
    pub configured: bool,
    /// Articles where the field ended up non-empty.
    pub filled: usize,
    /// ...of which came from the configured selector.
    pub from_selector: usize,
    /// ...of which came from a meta tag / JSON-LD / heuristic fallback.
    pub from_fallback: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiteReport {
    pub site: String,
    pub website_link: String,
    pub feed_links: Vec<String>,
    pub config_file: String,

    pub pagination_type: String,
    pub strategy: String,
    pub strategy_reason: String,

    pub started_at: String,
    pub finished_at: String,
    pub duration_secs: f64,

    pub pages_requested: usize,
    pub pages_ok: usize,
    pub pages_failed: usize,
    pub stopped_because: String,

    pub links_found: usize,
    pub links_duplicate: usize,
    pub articles_attempted: usize,
    pub articles_saved: usize,
    pub articles_saved_with_date: usize,
    pub articles_failed: usize,
    pub skipped_out_of_range: usize,
    pub skipped_no_date: usize,
    pub internal_links_total: usize,

    pub dates_parsed: usize,
    pub dates_unparsed: usize,
    pub unparsed_date_samples: Vec<String>,

    pub fields: BTreeMap<String, FieldStat>,
    pub bad_selectors: BTreeSet<String>,
    pub unused_selectors: Vec<String>,

    pub error_counts: BTreeMap<String, usize>,
    pub error_samples: Vec<String>,
    pub notes: Vec<String>,

    pub status: String,
    pub health: f64,
}

impl SiteReport {
    pub fn new(cfg: &SiteConfig) -> Self {
        let mut fields = BTreeMap::new();
        let configured: BTreeMap<&str, bool> = [
            ("title", !cfg.title_selector.is_empty()),
            ("description", !cfg.description_selector.is_empty()),
            ("author", !cfg.author_selector.is_empty()),
            ("publish_date", !cfg.publish_date_selector.is_empty()),
            ("images", !cfg.images_selector.is_empty()),
            ("comments", !cfg.comments_selector.is_empty()),
            ("internal_links", !cfg.internal_links_selector.is_empty()),
        ]
        .into_iter()
        .collect();

        for f in FIELDS {
            fields.insert(
                f.to_string(),
                FieldStat {
                    configured: *configured.get(f).unwrap_or(&false),
                    ..Default::default()
                },
            );
        }

        Self {
            site: cfg.name.clone(),
            website_link: cfg.website_link.clone(),
            feed_links: cfg.feeds(),
            config_file: cfg.source_file.display().to_string(),
            pagination_type: cfg.pagination_type.clone(),
            strategy: String::new(),
            strategy_reason: String::new(),
            started_at: String::new(),
            finished_at: String::new(),
            duration_secs: 0.0,
            pages_requested: 0,
            pages_ok: 0,
            pages_failed: 0,
            stopped_because: String::new(),
            links_found: 0,
            links_duplicate: 0,
            articles_attempted: 0,
            articles_saved: 0,
            articles_saved_with_date: 0,
            articles_failed: 0,
            skipped_out_of_range: 0,
            skipped_no_date: 0,
            internal_links_total: 0,
            dates_parsed: 0,
            dates_unparsed: 0,
            unparsed_date_samples: Vec::new(),
            fields,
            bad_selectors: BTreeSet::new(),
            unused_selectors: Vec::new(),
            error_counts: BTreeMap::new(),
            error_samples: Vec::new(),
            notes: Vec::new(),
            status: "not_run".to_string(),
            health: 0.0,
        }
    }

    pub fn record_error(&mut self, url: &str, message: &str) {
        *self.error_counts.entry(bucket(message)).or_insert(0) += 1;
        if self.error_samples.len() < 5 {
            self.error_samples.push(format!("{url} -> {message}"));
        }
    }

    pub fn record_field(&mut self, field: &str, filled: bool, source: Option<&str>) {
        let Some(stat) = self.fields.get_mut(field) else {
            return;
        };
        if !filled {
            return;
        }
        stat.filled += 1;
        match source {
            Some("selector") => stat.from_selector += 1,
            Some(_) => stat.from_fallback += 1,
            None => {}
        }
    }

    /// Selectors that were configured but never produced a value - the most
    /// actionable output of the whole run.
    pub fn finalize(&mut self) {
        self.unused_selectors = FIELDS
            .iter()
            .filter(|f| {
                self.fields
                    .get(**f)
                    .map(|s| s.configured && s.from_selector == 0)
                    .unwrap_or(false)
            })
            .map(|f| f.to_string())
            .collect();

        let fetch_score = if self.articles_attempted == 0 {
            0.0
        } else {
            self.articles_saved as f64 / (self.articles_saved + self.articles_failed).max(1) as f64
        };

        let configured: Vec<&FieldStat> = self.fields.values().filter(|s| s.configured).collect();
        let coverage = if self.articles_saved == 0 || configured.is_empty() {
            0.0
        } else {
            let sum: f64 = configured
                .iter()
                .map(|s| (s.filled as f64 / self.articles_saved as f64).min(1.0))
                .sum();
            sum / configured.len() as f64
        };

        self.health = if self.articles_saved == 0 {
            0.0
        } else {
            ((0.5 * fetch_score + 0.5 * coverage) * 100.0).round()
        };

        self.status = if self.articles_saved == 0 {
            "failed".to_string()
        } else if self.health >= 70.0 && self.articles_failed == 0 {
            "ok".to_string()
        } else {
            "partial".to_string()
        };
    }

    /// One-line log-friendly summary with counts and percentages.
    pub fn stats_line(&self) -> String {
        let n = self.articles_saved;
        format!(
            "pages {}/{} ({}), links {} ({} dup, {}), articles {}/{} ({}), failed {}, \
             dated {}/{} ({}), titles {} ({}), authors {} ({}), internal links {} \
             ({:.1}/art, {} of articles)",
            self.pages_ok,
            self.pages_requested,
            pct_of(self.pages_ok, self.pages_requested),
            self.links_found,
            self.links_duplicate,
            pct_of(self.links_duplicate, self.links_found),
            self.articles_saved,
            self.articles_attempted,
            pct_of(self.articles_saved, self.articles_attempted),
            self.articles_failed,
            self.articles_saved_with_date,
            n,
            pct_of(self.articles_saved_with_date, n),
            self.fields.get("title").map(|s| s.filled).unwrap_or(0),
            pct_of(self.fields.get("title").map(|s| s.filled).unwrap_or(0), n),
            self.fields.get("author").map(|s| s.filled).unwrap_or(0),
            pct_of(self.fields.get("author").map(|s| s.filled).unwrap_or(0), n),
            self.internal_links_total,
            self.internal_links_total as f64 / n.max(1) as f64,
            pct_of(self.internal_link_articles(), n),
        )
    }

    /// How many saved articles carry at least one internal link.
    pub fn internal_link_articles(&self) -> usize {
        self.fields
            .get("internal_links")
            .map(|s| s.filled)
            .unwrap_or(0)
    }

    pub fn fatal(cfg: &SiteConfig, message: &str) -> Self {
        let mut r = Self::new(cfg);
        r.status = "failed".to_string();
        r.notes.push(format!("aborted: {message}"));
        r
    }
}

/// Group similar errors so the counts stay readable.
fn bucket(message: &str) -> String {
    let m = message.trim();
    if let Some(rest) = m.strip_prefix("HTTP ") {
        let code: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !code.is_empty() {
            return format!("HTTP {code}");
        }
    }
    for known in [
        "timeout",
        "connection failed",
        "too many redirects",
        "decode error",
        "body read failed",
    ] {
        if m.starts_with(known) {
            return known.to_string();
        }
    }
    m.chars().take(60).collect()
}

// ---------------------------------------------------------------------------
// Run level
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RunTotals {
    pub sites: usize,
    pub sites_ok: usize,
    pub sites_partial: usize,
    pub sites_failed: usize,
    pub pages_requested: usize,
    pub pages_ok: usize,
    pub links_found: usize,
    pub links_duplicate: usize,
    pub articles_attempted: usize,
    pub articles_saved: usize,
    pub articles_saved_with_date: usize,
    pub articles_failed: usize,
    pub skipped_out_of_range: usize,
    pub titles_filled: usize,
    pub authors_filled: usize,
    pub dates_parsed: usize,
    pub dates_unparsed: usize,
    pub internal_links_total: usize,
    pub internal_link_articles: usize,
}

impl RunTotals {
    /// One-line log-friendly summary with counts and percentages.
    pub fn stats_line(&self) -> String {
        let n = self.articles_saved;
        format!(
            "pages {}/{} ({}), links {} ({} dup, {}), articles {}/{} ({}), failed {}, \
             dated {}/{} ({}), titles {} ({}), authors {} ({}), internal links {} \
             ({:.1}/art, {} of articles)",
            self.pages_ok,
            self.pages_requested,
            pct_of(self.pages_ok, self.pages_requested),
            self.links_found,
            self.links_duplicate,
            pct_of(self.links_duplicate, self.links_found),
            self.articles_saved,
            self.articles_attempted,
            pct_of(self.articles_saved, self.articles_attempted),
            self.articles_failed,
            self.articles_saved_with_date,
            n,
            pct_of(self.articles_saved_with_date, n),
            self.titles_filled,
            pct_of(self.titles_filled, n),
            self.authors_filled,
            pct_of(self.authors_filled, n),
            self.internal_links_total,
            self.internal_links_total as f64 / n.max(1) as f64,
            pct_of(self.internal_link_articles, n),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub started_at: String,
    pub finished_at: String,
    pub duration_secs: f64,
    pub options: BTreeMap<String, String>,
    pub totals: RunTotals,
    pub sites: Vec<SiteReport>,
}

impl RunSummary {
    pub fn build(
        started_at: String,
        finished_at: String,
        duration_secs: f64,
        options: BTreeMap<String, String>,
        mut sites: Vec<SiteReport>,
    ) -> Self {
        sites.sort_by(|a, b| {
            rank(&a.status)
                .cmp(&rank(&b.status))
                .then(
                    a.health
                        .partial_cmp(&b.health)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(a.site.cmp(&b.site))
        });

        let totals = RunTotals {
            sites: sites.len(),
            sites_ok: sites.iter().filter(|s| s.status == "ok").count(),
            sites_partial: sites.iter().filter(|s| s.status == "partial").count(),
            sites_failed: sites.iter().filter(|s| s.status == "failed").count(),
            pages_requested: sites.iter().map(|s| s.pages_requested).sum(),
            pages_ok: sites.iter().map(|s| s.pages_ok).sum(),
            links_found: sites.iter().map(|s| s.links_found).sum(),
            links_duplicate: sites.iter().map(|s| s.links_duplicate).sum(),
            articles_attempted: sites.iter().map(|s| s.articles_attempted).sum(),
            articles_saved: sites.iter().map(|s| s.articles_saved).sum(),
            articles_saved_with_date: sites.iter().map(|s| s.articles_saved_with_date).sum(),
            articles_failed: sites.iter().map(|s| s.articles_failed).sum(),
            skipped_out_of_range: sites.iter().map(|s| s.skipped_out_of_range).sum(),
            titles_filled: sites
                .iter()
                .map(|s| s.fields.get("title").map(|f| f.filled).unwrap_or(0))
                .sum(),
            authors_filled: sites
                .iter()
                .map(|s| s.fields.get("author").map(|f| f.filled).unwrap_or(0))
                .sum(),
            dates_parsed: sites.iter().map(|s| s.dates_parsed).sum(),
            dates_unparsed: sites.iter().map(|s| s.dates_unparsed).sum(),
            internal_links_total: sites.iter().map(|s| s.internal_links_total).sum(),
            internal_link_articles: sites.iter().map(|s| s.internal_link_articles()).sum(),
        };

        Self {
            started_at,
            finished_at,
            duration_secs,
            options,
            totals,
            sites,
        }
    }

    pub fn to_markdown(&self) -> String {
        let t = &self.totals;
        let mut md = String::new();
        md.push_str("# Scrape run summary\n\n");
        md.push_str(&format!(
            "- started: `{}`\n- finished: `{}`\n- duration: {:.1}s\n\n",
            self.started_at, self.finished_at, self.duration_secs
        ));

        md.push_str("## Options\n\n");
        for (k, v) in &self.options {
            md.push_str(&format!("- `{k}` = `{v}`\n"));
        }

        md.push_str(&format!(
            "\n## Totals\n\n\
             | metric | value | share |\n|---|---:|---:|\n\
             | sites | {} | {} ok / {} partial / {} failed |\n\
             | feed pages ok | {} of {} requested | {} |\n\
             | links found | {} | {} duplicates |\n\
             | articles attempted | {} | - |\n\
             | articles saved | {} | {} of attempted |\n\
             | articles failed | {} | {} of attempted |\n\
             | articles with parsed date | {} of {} saved | {} |\n\
             | titles filled | {} of {} saved | {} |\n\
             | authors filled | {} of {} saved | {} |\n\
             | internal links collected | {} ({:.1} per article) | on {} of saved articles |\n\
             | skipped (outside date range) | {} | - |\n\
             | dates parsed / unparsed | {} / {} | {} parsed |\n",
            t.sites,
            t.sites_ok,
            t.sites_partial,
            t.sites_failed,
            t.pages_ok,
            t.pages_requested,
            pct_of(t.pages_ok, t.pages_requested),
            t.links_found,
            t.links_duplicate,
            t.articles_attempted,
            t.articles_saved,
            pct_of(t.articles_saved, t.articles_attempted),
            t.articles_failed,
            pct_of(t.articles_failed, t.articles_attempted),
            t.articles_saved_with_date,
            t.articles_saved,
            pct_of(t.articles_saved_with_date, t.articles_saved),
            t.titles_filled,
            t.articles_saved,
            pct_of(t.titles_filled, t.articles_saved),
            t.authors_filled,
            t.articles_saved,
            pct_of(t.authors_filled, t.articles_saved),
            t.internal_links_total,
            t.internal_links_total as f64 / t.articles_saved.max(1) as f64,
            pct_of(t.internal_link_articles, t.articles_saved),
            t.skipped_out_of_range,
            t.dates_parsed,
            t.dates_unparsed,
            pct_of(t.dates_parsed, t.dates_parsed + t.dates_unparsed),
        ));

        md.push_str("\n## Per site\n\nWorst first. `health` blends fetch success with how many configured selectors produced a value.\n\n");
        md.push_str("| site | status | health | pages | links (dup) | attempted | saved | failed | dated | title | author | int.links | pagination |\n");
        md.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|\n");
        for s in &self.sites {
            let pages = format!("{}/{}", s.pages_ok, s.pages_requested);
            let links = format!("{} ({})", s.links_found, s.links_duplicate);
            let saved = format!(
                "{} ({})",
                s.articles_saved,
                pct_of(s.articles_saved, s.articles_attempted)
            );
            let failed = format!(
                "{} ({})",
                s.articles_failed,
                pct_of(s.articles_failed, s.articles_attempted)
            );
            let dated = format!(
                "{}/{} ({})",
                s.articles_saved_with_date,
                s.articles_saved,
                pct_of(s.articles_saved_with_date, s.articles_saved)
            );
            let title = s.fields.get("title").map(|f| f.filled).unwrap_or(0);
            let title = format!("{title} ({})", pct_of(title, s.articles_saved));
            let author = s.fields.get("author").map(|f| f.filled).unwrap_or(0);
            let author = format!("{author} ({})", pct_of(author, s.articles_saved));
            let intl = format!(
                "{} ({:.1}/a)",
                s.internal_links_total,
                s.internal_links_total as f64 / s.articles_saved.max(1) as f64
            );
            md.push_str(&format!(
                "| {} | {} | {:.0} | {pages} | {links} | {} | {saved} | {failed} | {dated} | {title} | {author} | {intl} | {} |\n",
                s.site,
                s.status,
                s.health,
                s.articles_attempted,
                s.strategy,
            ));
        }

        let broken: Vec<&SiteReport> = self.sites.iter().filter(|s| s.status == "failed").collect();
        if !broken.is_empty() {
            md.push_str("\n## Sites that produced nothing\n\n");
            for s in broken {
                let why = if !s.notes.is_empty() {
                    s.notes.join("; ")
                } else if !s.error_counts.is_empty() {
                    s.error_counts
                        .iter()
                        .map(|(k, v)| format!("{k} x{v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                } else if s.links_found == 0 {
                    "feed page loaded but article_link_selector matched no links".to_string()
                } else {
                    "unknown".to_string()
                };
                md.push_str(&format!("- **{}** - {why}\n", s.site));
            }
        }

        let dead: Vec<&SiteReport> = self
            .sites
            .iter()
            .filter(|s| s.articles_saved > 0 && !s.unused_selectors.is_empty())
            .collect();
        if !dead.is_empty() {
            md.push_str("\n## Selectors that never matched\n\nConfigured in the JSON, matched nothing on any article.\n\n");
            for s in dead {
                md.push_str(&format!(
                    "- **{}** - {}\n",
                    s.site,
                    s.unused_selectors.join(", ")
                ));
            }
        }

        let invalid: Vec<&SiteReport> = self
            .sites
            .iter()
            .filter(|s| !s.bad_selectors.is_empty())
            .collect();
        if !invalid.is_empty() {
            md.push_str("\n## Selectors that would not parse as CSS\n\n");
            for s in invalid {
                for bad in &s.bad_selectors {
                    md.push_str(&format!("- **{}** - `{bad}`\n", s.site));
                }
            }
        }

        md
    }
}

/// `a of b (pct%)`, handling b == 0.
pub fn pct_of(a: usize, b: usize) -> String {
    if b == 0 {
        return "n/a".to_string();
    }
    format!("{:.1}%", a as f64 * 100.0 / b as f64)
}

fn rank(status: &str) -> u8 {
    match status {
        "failed" => 0,
        "partial" => 1,
        "ok" => 2,
        _ => 3,
    }
}
