mod config;
mod dates;
mod extract;
mod http;
mod output;
mod pagination;
mod summary;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::{ArgAction, Parser, ValueEnum};
use futures::stream::{self, StreamExt};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use config::SiteConfig;
use extract::ArticleSelectors;
use output::{Article, Format, SiteWriter};
use pagination::Strategy;
use summary::{RunSummary, SiteReport};

const DEFAULT_UA: &str = "Mozilla/5.0 (compatible; news-scraper/0.1; +https://example.invalid/bot)";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutFormat {
    /// One JSON object per line in articles.jsonl (default).
    Jsonl,
    /// One .json file per article.
    Files,
}

impl From<OutFormat> for Format {
    fn from(f: OutFormat) -> Self {
        match f {
            OutFormat::Jsonl => Format::Jsonl,
            OutFormat::Files => Format::Files,
        }
    }
}

/// Scrape news sites described by merged selector JSON files.
#[derive(Debug, Parser)]
#[command(name = "news-scraper")]
struct Cli {
    /// Folder of merged selector files (output of merge_selectors.py).
    #[arg(long, default_value = "selectors_merged")]
    config_dir: PathBuf,

    /// Where to write scraped articles and reports.
    #[arg(long, default_value = "output")]
    out: PathBuf,

    /// Only scrape sites whose name or URL contains this. Repeatable.
    #[arg(long = "site")]
    sites: Vec<String>,

    /// Maximum feed pages to walk per feed.
    #[arg(long, default_value_t = 3)]
    max_pages: usize,

    /// Stop a site after this many saved articles.
    #[arg(long)]
    max_articles: Option<usize>,

    /// Only keep articles published on or after this date (YYYY-MM-DD).
    #[arg(long)]
    since: Option<String>,

    /// Only keep articles published on or before this date (YYYY-MM-DD).
    #[arg(long)]
    until: Option<String>,

    /// With --since, stop paginating after this many consecutive pages where
    /// every dated article was older than the cutoff.
    #[arg(long, default_value_t = 2)]
    stale_pages: usize,

    /// Drop articles whose publish date could not be parsed.
    #[arg(long)]
    require_date: bool,

    /// Articles fetched in parallel within one site.
    #[arg(long, default_value_t = 8)]
    concurrency: usize,

    /// Sites scraped in parallel.
    #[arg(long, default_value_t = 8)]
    site_concurrency: usize,

    /// Minimum spacing between the starts of one site's article requests, in
    /// milliseconds (0 disables pacing).
    #[arg(long, default_value_t = 250)]
    delay_ms: u64,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 25)]
    timeout_secs: u64,

    /// Retries for timeouts, connection errors, 429 and 5xx.
    #[arg(long, default_value_t = 2)]
    retries: usize,

    #[arg(long, default_value_t = DEFAULT_UA.to_string())]
    user_agent: String,

    #[arg(long, value_enum, default_value_t = OutFormat::Jsonl)]
    format: OutFormat,

    /// Keep previously scraped articles and skip URLs already saved.
    #[arg(long)]
    resume: bool,

    /// Walk feeds and report what would be scraped without fetching articles.
    #[arg(long)]
    dry_run: bool,

    /// Print the loaded configs and the pagination plan, then exit.
    #[arg(long)]
    list: bool,

    /// -v for debug, -vv for trace.
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let mut configs = config::load_dir(&cli.config_dir)?;
    let loaded = configs.len();
    if !cli.sites.is_empty() {
        let needles: Vec<String> = cli.sites.iter().map(|s| s.to_lowercase()).collect();
        configs.retain(|c| {
            let hay = format!("{} {}", c.name, c.website_link).to_lowercase();
            needles.iter().any(|n| hay.contains(n))
        });
    }
    if configs.is_empty() {
        return Err(anyhow!(
            "no sites to scrape ({loaded} config(s) in {}, none matched the --site filter)",
            cli.config_dir.display()
        ));
    }
    info!(
        "loaded {} site config(s) from {}{}",
        configs.len(),
        cli.config_dir.display(),
        if configs.len() == loaded {
            String::new()
        } else {
            format!(" (filtered from {loaded})")
        }
    );

    if cli.list {
        for cfg in &configs {
            let plan = pagination::plan(cfg);
            let missing = cfg.unconfigured();
            println!(
                "{:<28} {:<12} {:<40} missing: {}",
                cfg.name,
                plan.strategy.label(),
                truncate(&cfg.feed_link, 40),
                if missing.is_empty() {
                    "-".to_string()
                } else {
                    missing.join(",")
                }
            );
        }
        return Ok(());
    }

    let since = cli
        .since
        .as_deref()
        .map(|s| day_bound(s, false))
        .transpose()?;
    let until = cli
        .until
        .as_deref()
        .map(|s| day_bound(s, true))
        .transpose()?;
    if let (Some(a), Some(b)) = (since, until) {
        if a > b {
            return Err(anyhow!("--since is after --until"));
        }
    }

    let client = http::build_client(&cli.user_agent, Duration::from_secs(cli.timeout_secs))?;
    std::fs::create_dir_all(&cli.out)
        .with_context(|| format!("cannot create output dir {}", cli.out.display()))?;

    let started_at = Utc::now();
    let clock = Instant::now();
    let site_conc = cli.site_concurrency.max(1);
    let cli = Arc::new(cli);

    let reports: Vec<SiteReport> = stream::iter(configs)
        .map(|cfg| {
            let client = client.clone();
            let cli = Arc::clone(&cli);
            async move {
                match scrape_site(&client, &cli, &cfg, since, until).await {
                    Ok(report) => report,
                    Err(e) => {
                        warn!(site = %cfg.name, "site aborted: {e}");
                        SiteReport::fatal(&cfg, &e.to_string())
                    }
                }
            }
        })
        .buffer_unordered(site_conc)
        .collect()
        .await;

    let finished_at = Utc::now();
    let summary = RunSummary::build(
        started_at.to_rfc3339(),
        finished_at.to_rfc3339(),
        clock.elapsed().as_secs_f64(),
        options_map(&cli),
        reports,
    );

    let json_path = cli.out.join("run_summary.json");
    std::fs::write(&json_path, serde_json::to_vec_pretty(&summary)?)
        .with_context(|| format!("cannot write {}", json_path.display()))?;
    let md_path = cli.out.join("run_summary.md");
    std::fs::write(&md_path, summary.to_markdown())
        .with_context(|| format!("cannot write {}", md_path.display()))?;

    for site in &summary.sites {
        let dir = cli.out.join(output::sanitize(&site.site));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("summary.json"), serde_json::to_vec_pretty(site)?)?;
    }

    print_console_summary(&summary);
    info!("totals: {}", summary.totals.stats_line());
    info!(
        "reports written to {} and {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------

/// In-flight fetch of a feed page. Spawned as its own task so the next page
/// downloads while the current one is parsed and its articles are scraped.
type FeedFetch = tokio::task::JoinHandle<(String, Result<http::Fetched, anyhow::Error>)>;

/// Kick off the fetch of the next feed page in the background.
fn spawn_feed_fetch(
    client: &reqwest::Client,
    url: Option<String>,
    retries: usize,
) -> Option<FeedFetch> {
    let url = url?;
    let client = client.clone();
    Some(tokio::spawn(async move {
        let result = http::fetch(&client, &url, retries).await;
        (url, result)
    }))
}

/// Await a spawned feed-page fetch; `None` means there was no page to fetch.
async fn finish_feed_fetch(
    handle: Option<FeedFetch>,
) -> Option<(String, Result<http::Fetched, anyhow::Error>)> {
    match handle?.await {
        Ok(pair) => Some(pair),
        Err(e) => Some((String::new(), Err(anyhow!("feed fetch task failed: {e}")))),
    }
}

async fn scrape_site(
    client: &reqwest::Client,
    cli: &Cli,
    cfg: &SiteConfig,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<SiteReport> {
    let clock = Instant::now();
    let started = Utc::now();
    let mut rep = SiteReport::new(cfg);
    rep.started_at = started.to_rfc3339();

    let plan = pagination::plan(cfg);
    rep.strategy = plan.strategy.label().to_string();
    rep.strategy_reason = plan.reason.clone();
    rep.notes.extend(plan.notes.clone());

    let feeds = cfg.feeds();
    if feeds.is_empty() {
        rep.notes.push("no feed_link in the config".to_string());
        rep.stopped_because = "no feed_link".to_string();
        finish(&mut rep, started, clock);
        return Ok(rep);
    }
    if cfg.article_link_selector.trim().is_empty() {
        rep.notes
            .push("no article_link_selector in the config".to_string());
        rep.stopped_because = "no article_link_selector".to_string();
        finish(&mut rep, started, clock);
        return Ok(rep);
    }

    let sels = ArticleSelectors::from(cfg);
    let site_name = rep.site.clone();
    let (mut writer, mut seen) =
        SiteWriter::new(&cli.out, &rep.site, cli.format.into(), cli.resume)?;
    if !seen.is_empty() {
        info!(site = %rep.site, "resuming, {} URL(s) already saved", seen.len());
    }

    info!(
        site = %rep.site,
        "start: {} feed(s), pagination {} ({})",
        feeds.len(),
        rep.strategy,
        rep.strategy_reason
    );

    'feeds: for feed in &feeds {
        let mut page_idx: usize = 1;
        let mut stale_streak: usize = 0;
        let mut pattern_fallback_used = false;
        let first = match &plan.strategy {
            Strategy::UrlPattern { pattern } => {
                pagination::render(pattern, plan.start, base_for(cfg, feed))
            }
            _ => feed.clone(),
        };
        let mut prefetched = spawn_feed_fetch(client, Some(first), cli.retries);

        while page_idx <= cli.max_pages {
            let Some((page_url, fetched)) = finish_feed_fetch(prefetched).await else {
                rep.stopped_because = "no next page link".to_string();
                break;
            };

            rep.pages_requested += 1;
            let fetched = match fetched {
                Ok(f) => f,
                Err(e) => {
                    rep.pages_failed += 1;
                    rep.record_error(&page_url, &e.to_string());
                    warn!(site = %rep.site, "feed page {page_idx} failed ({page_url}): {e}");
                    if page_idx == 1 && !pattern_fallback_used && is_pattern(&plan.strategy) {
                        pattern_fallback_used = true;
                        rep.notes.push(format!(
                            "pattern URL {page_url} failed; fell back to feed_link"
                        ));
                        prefetched = spawn_feed_fetch(client, Some(feed.clone()), cli.retries);
                        continue;
                    }
                    rep.stopped_because = format!("feed page failed: {e}");
                    break;
                }
            };
            rep.pages_ok += 1;

            let body = fetched.body;
            let base = fetched.url.clone();
            let art_sel = cfg.article_link_selector.clone();
            let next_sel = cfg.next_page_selector.clone();
            let feed_page = tokio::task::spawn_blocking(move || {
                extract::parse_feed(&body, &base, &art_sel, &next_sel)
            })
            .await
            .map_err(|e| anyhow!("feed parsing panicked: {e}"))?;

            for bad in &feed_page.bad_selectors {
                rep.bad_selectors.insert(bad.clone());
            }

            let found = feed_page.links.len();
            rep.links_found += found;
            let mut fresh: Vec<String> = Vec::new();
            for link in feed_page.links {
                if seen.insert(link.clone()) {
                    fresh.push(link);
                }
            }
            rep.links_duplicate += found - fresh.len();

            if found == 0 {
                warn!(site = %rep.site, "page {page_idx}: article_link_selector matched no links ({page_url})");
                if page_idx == 1 && !pattern_fallback_used && is_pattern(&plan.strategy) {
                    pattern_fallback_used = true;
                    rep.notes.push(format!(
                        "pattern URL {page_url} had no links; fell back to feed_link"
                    ));
                    prefetched = spawn_feed_fetch(client, Some(feed.clone()), cli.retries);
                    continue;
                }
                rep.stopped_because = "no article links on page".to_string();
                break;
            }
            if fresh.is_empty() {
                info!(site = %rep.site, "page {page_idx}: {found} link(s), all already seen - stopping");
                rep.stopped_because = "page repeated links from an earlier page".to_string();
                break;
            }

            if let Some(max) = cli.max_articles {
                let left = max.saturating_sub(rep.articles_saved);
                if left == 0 {
                    rep.stopped_because = "--max-articles reached".to_string();
                    break 'feeds;
                }
                fresh.truncate(left);
            }

            info!(
                site = %rep.site,
                "page {page_idx}: {} new link(s) of {found} ({page_url})",
                fresh.len()
            );

            if cli.dry_run {
                rep.articles_attempted += fresh.len();
            } else {
                // One shared pacer spaces request *starts* evenly instead of
                // sleeping i*delay before each item: with several requests in
                // flight, the wait for one slot overlaps other downloads.
                let pacer = Arc::new(tokio::sync::Mutex::new(http::Pacer::new(cli.delay_ms)));
                let outcomes: Vec<(String, Result<Article, String>)> =
                    stream::iter(fresh.iter().cloned())
                        .map(|link| {
                            let client = client.clone();
                            let sels = sels.clone();
                            let site = site_name.clone();
                            let source_page = page_url.clone();
                            let retries = cli.retries;
                            let pacer = Arc::clone(&pacer);
                            async move {
                                let slot = pacer.lock().await.reserve();
                                let now = tokio::time::Instant::now();
                                if slot > now {
                                    tokio::time::sleep_until(slot).await;
                                }
                                let r = fetch_article(
                                    &client,
                                    &link,
                                    &sels,
                                    retries,
                                    &site,
                                    &source_page,
                                )
                                .await;
                                (link, r)
                            }
                        })
                        .buffer_unordered(cli.concurrency.max(1))
                        .collect()
                        .await;

                let mut dated = 0usize;
                let mut too_old = 0usize;

                for (link, outcome) in outcomes {
                    rep.articles_attempted += 1;
                    let article = match outcome {
                        Ok(a) => a,
                        Err(e) => {
                            rep.articles_failed += 1;
                            rep.record_error(&link, &e);
                            debug!(site = %rep.site, "article failed ({link}): {e}");
                            continue;
                        }
                    };

                    match article.publish_dt {
                        Some(dt) => {
                            rep.dates_parsed += 1;
                            dated += 1;
                            if since.map(|s| dt < s).unwrap_or(false) {
                                too_old += 1;
                                rep.skipped_out_of_range += 1;
                                continue;
                            }
                            if until.map(|u| dt > u).unwrap_or(false) {
                                rep.skipped_out_of_range += 1;
                                continue;
                            }
                        }
                        None => {
                            rep.dates_unparsed += 1;
                            if let Some(raw) = article.publish_date_raw.as_deref() {
                                if rep.unparsed_date_samples.len() < 5
                                    && !rep.unparsed_date_samples.iter().any(|s| s == raw)
                                {
                                    rep.unparsed_date_samples.push(raw.to_string());
                                }
                            }
                            if cli.require_date {
                                rep.skipped_no_date += 1;
                                continue;
                            }
                        }
                    }

                    for (field, value_present) in [
                        ("title", article.title.is_some()),
                        ("description", article.description.is_some()),
                        ("author", article.author.is_some()),
                        ("publish_date", article.publish_date_raw.is_some()),
                        ("images", !article.images.is_empty()),
                        ("comments", !article.comments.is_empty()),
                        ("internal_links", !article.internal_links.is_empty()),
                    ] {
                        rep.record_field(
                            field,
                            value_present,
                            article.field_sources.get(field).map(|s| s.as_str()),
                        );
                    }

                    if article.publish_dt.is_some() {
                        rep.articles_saved_with_date += 1;
                    }
                    rep.internal_links_total += article.internal_links.len();
                    writer.write(&article)?;
                    rep.articles_saved += 1;
                }

                if since.is_some() && dated > 0 && too_old == dated {
                    stale_streak += 1;
                    if stale_streak >= cli.stale_pages.max(1) {
                        info!(site = %rep.site, "every dated article on the last {stale_streak} page(s) predates --since - stopping");
                        rep.stopped_because = "reached articles older than --since".to_string();
                        break;
                    }
                } else {
                    stale_streak = 0;
                }
            }

            // Start the next feed page now so it downloads while we scrape.
            let next = match &plan.strategy {
                Strategy::UrlPattern { pattern } => Some(pagination::render(
                    pattern,
                    plan.start + page_idx as i64,
                    base_for(cfg, feed),
                )),
                Strategy::NextButton { .. } => feed_page.next_url.clone(),
                Strategy::Single => None,
            };
            prefetched = spawn_feed_fetch(client, next, cli.retries);
            page_idx += 1;

            if page_idx > cli.max_pages && rep.stopped_because.is_empty() {
                rep.stopped_because = "--max-pages reached".to_string();
            }
        }
    }

    let dir = writer.dir().to_path_buf();
    writer.finish()?;
    finish(&mut rep, started, clock);

    info!(
        site = %rep.site,
        "done: {} | health {:.0} -> {}",
        rep.stats_line(),
        rep.health,
        dir.display()
    );
    Ok(rep)
}

async fn fetch_article(
    client: &reqwest::Client,
    url: &str,
    sels: &ArticleSelectors,
    retries: usize,
    site: &str,
    source_page: &str,
) -> Result<Article, String> {
    let fetched = http::fetch(client, url, retries)
        .await
        .map_err(|e| e.to_string())?;

    let body = fetched.body;
    let final_url = fetched.url.clone();
    let sels = sels.clone();
    let for_parser = final_url.clone();
    let extracted =
        tokio::task::spawn_blocking(move || extract::extract_article(&body, &sels, &for_parser))
            .await
            .map_err(|e| format!("article parsing panicked: {e}"))?;

    let publish_dt = extracted
        .publish_date_raw
        .as_deref()
        .and_then(dates::parse_date);

    Ok(Article {
        site: site.to_string(),
        url: final_url,
        source_page: source_page.to_string(),
        http_status: fetched.status,
        fetched_at: Utc::now().to_rfc3339(),
        word_count: extracted
            .description
            .as_deref()
            .map(|d| d.split_whitespace().count())
            .unwrap_or(0),
        title: extracted.title,
        author: extracted.author,
        publish_date: publish_dt.map(|d| d.to_rfc3339()),
        publish_date_raw: extracted.publish_date_raw,
        description: extracted.description,
        images: extracted.images,
        comments: extracted.comments,
        internal_links: extracted.internal_links,
        field_sources: extracted.sources,
        publish_dt,
    })
}

// ---------------------------------------------------------------------------

fn finish(rep: &mut SiteReport, started: DateTime<Utc>, clock: Instant) {
    let _ = started;
    rep.finished_at = Utc::now().to_rfc3339();
    rep.duration_secs = clock.elapsed().as_secs_f64();
    rep.finalize();
}

fn is_pattern(s: &Strategy) -> bool {
    matches!(s, Strategy::UrlPattern { .. })
}

/// Base URL for resolving a relative pagination pattern.
fn base_for<'a>(cfg: &'a SiteConfig, feed: &'a str) -> &'a str {
    if cfg.website_link.is_empty() {
        feed
    } else {
        &cfg.website_link
    }
}

fn day_bound(s: &str, end_of_day: bool) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date '{s}', expected YYYY-MM-DD"))?;
    let naive = if end_of_day {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .ok_or_else(|| anyhow!("invalid date '{s}'"))?;
    Ok(Utc.from_utc_datetime(&naive))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!(
            "{}...",
            s.chars().take(n.saturating_sub(3)).collect::<String>()
        )
    }
}

fn options_map(cli: &Cli) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("config_dir".into(), cli.config_dir.display().to_string());
    m.insert("out".into(), cli.out.display().to_string());
    m.insert("max_pages".into(), cli.max_pages.to_string());
    m.insert(
        "max_articles".into(),
        cli.max_articles
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unlimited".into()),
    );
    m.insert(
        "since".into(),
        cli.since.clone().unwrap_or_else(|| "-".into()),
    );
    m.insert(
        "until".into(),
        cli.until.clone().unwrap_or_else(|| "-".into()),
    );
    m.insert("stale_pages".into(), cli.stale_pages.to_string());
    m.insert("require_date".into(), cli.require_date.to_string());
    m.insert("concurrency".into(), cli.concurrency.to_string());
    m.insert("site_concurrency".into(), cli.site_concurrency.to_string());
    m.insert("delay_ms".into(), cli.delay_ms.to_string());
    m.insert("timeout_secs".into(), cli.timeout_secs.to_string());
    m.insert("retries".into(), cli.retries.to_string());
    m.insert("resume".into(), cli.resume.to_string());
    m.insert("dry_run".into(), cli.dry_run.to_string());
    m
}

fn print_console_summary(s: &RunSummary) {
    let t = &s.totals;
    println!();
    println!("================ run summary ================");
    println!(
        "sites      : {} ({} ok, {} partial, {} failed)",
        t.sites, t.sites_ok, t.sites_partial, t.sites_failed
    );
    println!(
        "feed pages : {} ok of {} requested ({})",
        t.pages_ok,
        t.pages_requested,
        summary::pct_of(t.pages_ok, t.pages_requested)
    );
    println!(
        "links      : {} found, {} duplicates ({}), {} article(s) attempted",
        t.links_found,
        t.links_duplicate,
        summary::pct_of(t.links_duplicate, t.links_found),
        t.articles_attempted
    );
    println!(
        "articles   : {} saved of {} attempted ({}), {} failed, {} outside date range",
        t.articles_saved,
        t.articles_attempted,
        summary::pct_of(t.articles_saved, t.articles_attempted),
        t.articles_failed,
        t.skipped_out_of_range
    );
    println!(
        "titles     : {} on saved articles ({})",
        t.titles_filled,
        summary::pct_of(t.titles_filled, t.articles_saved)
    );
    println!(
        "authors    : {} on saved articles ({})",
        t.authors_filled,
        summary::pct_of(t.authors_filled, t.articles_saved)
    );
    println!(
        "pub dates  : {} of {} saved parsed ({})",
        t.articles_saved_with_date,
        t.articles_saved,
        summary::pct_of(t.articles_saved_with_date, t.articles_saved)
    );
    println!(
        "int. links : {} total ({:.1}/article, on {} of saved articles)",
        t.internal_links_total,
        t.internal_links_total as f64 / t.articles_saved.max(1) as f64,
        summary::pct_of(t.internal_link_articles, t.articles_saved)
    );
    println!("duration   : {:.1}s", s.duration_secs);

    let failed: Vec<&SiteReport> = s.sites.iter().filter(|x| x.status == "failed").collect();
    if !failed.is_empty() {
        println!("\nproduced nothing ({}):", failed.len());
        for site in failed.iter().take(15) {
            let why = site
                .notes
                .first()
                .cloned()
                .or_else(|| {
                    site.error_counts
                        .iter()
                        .next()
                        .map(|(k, v)| format!("{k} x{v}"))
                })
                .unwrap_or_else(|| site.stopped_because.clone());
            println!("  {:<28} {}", truncate(&site.site, 28), truncate(&why, 70));
        }
        if failed.len() > 15 {
            println!("  ... and {} more, see run_summary.md", failed.len() - 15);
        }
    }
    println!("=============================================");
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("news_scraper={level},warn")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
