# News scraper

Two pieces:

1. `merge_selectors.py` — folds the split selector files (`site.json` + `site(1).json`) into one file per site.
2. `news-scraper/` — a Rust CLI that walks each site's feed, paginates, scrapes the articles and reports how well it did.

## Layout

Put this next to your existing `selectors` folder:

```
.
├── selectors/            <- your 108 raw files
├── selectors_merged/     <- created by step 1
├── merge_selectors.py
├── news-scraper/
│   ├── Cargo.toml
│   └── src/
└── output/               <- created by step 2
```

## 1. Merge

```bash
python merge_selectors.py -i selectors -o selectors_merged
```

Files are grouped by the host in `website_link` (not by file name), so `en.irna.ir.json` and
`en.irna.ir(1).json` land in the same group regardless of the `(N)` convention. Within a group each
field takes the first non-empty value, base file before `(1)` before `(2)`.

The output keeps the original 14 fields and adds two:

- `extra_feed_links` — if two source files disagreed on `feed_link`, the site has more than one feed; the scraper walks all of them.
- `_meta` — source files, conflicting values, and which required fields are missing. The scraper ignores it.

It prints (and writes to `selectors_merged/_merge_report.json`) which sites were assembled from
several files, where values conflicted, and which sites are unusable because `feed_link` or
`article_link_selector` is empty. Add `--check` to analyse without writing.

## 2. Scrape

Needs Rust: <https://rustup.rs>. Then:

```bash
cd news-scraper
cargo build --release
```

The binary lands at `news-scraper/target/release/news-scraper` (`.exe` on Windows). Run it from the
folder that holds `selectors_merged`.

Sanity check first — prints the plan for every site and exits without a single request:

```bash
news-scraper --list
```

A small real run:

```bash
news-scraper --max-pages 2 --max-articles 20
```

Everything since a date, across more pages:

```bash
news-scraper --since 2026-09-01 --max-pages 25
```

One site, verbose, one JSON file per article:

```bash
news-scraper --site iz.ru --format files -v
```

### Options

| flag | meaning |
|---|---|
| `--config-dir <dir>` | merged selector files (default `selectors_merged`) |
| `--out <dir>` | output root (default `output`) |
| `--site <text>` | only sites whose name or URL contains this; repeatable |
| `--max-pages <n>` | feed pages per feed (default 3) |
| `--max-articles <n>` | stop a site after this many saved articles |
| `--since` / `--until` | `YYYY-MM-DD`, filters on parsed publish date |
| `--stale-pages <n>` | with `--since`, stop after N consecutive pages where every dated article is older (default 2) |
| `--require-date` | drop articles whose date could not be parsed |
| `--concurrency <n>` | articles in flight per site (default 8) |
| `--site-concurrency <n>` | sites in flight (default 8) |
| `--delay-ms <n>` | minimum spacing between the starts of one site's article requests, in ms (default 250) |
| `--timeout-secs`, `--retries` | per request (default 25s, 2 retries on timeout/429/5xx) |
| `--user-agent <ua>` | change the UA string |
| `--format jsonl\|files` | one file of JSON lines, or one file per article (default `jsonl`) |
| `--resume` | keep what is already there and skip URLs already saved |
| `--dry-run` | walk feeds, count links, fetch no articles |
| `--list` | print the plan and exit |
| `-v`, `-vv` | debug / trace logging (or set `RUST_LOG`) |

### Output

```
output/
├── run_summary.json          machine-readable, every site
├── run_summary.md            the readable one - start here
└── iz.ru/
    ├── articles.jsonl        one article per line
    ├── _seen_urls.txt        used by --resume
    └── summary.json          this site's report
```

Each article carries `title`, `description`, `author`, `publish_date` (normalised to RFC 3339),
`publish_date_raw` (exactly what was on the page), `images`, `comments`, `internal_links`,
`word_count`, plus `field_sources` — whether each value came from your selector or from a fallback.

### How pagination is decided

`pagination_type` is treated as a hint; what is used is whatever actually works, in this order:

1. **`pagination_pattern`** if it contains a `{placeholder}` — any name works (`{page}`, `{n}`, `{}`), replaced with `first_page_number`, then +1 each page. Relative patterns are resolved against `website_link`. If the pattern's first page 404s or yields no links, it falls back to `feed_link` once and continues from page 2.
2. **`next_page_selector`** — follows the href of the match on each page.
3. Otherwise only the feed page is read, and the site report says why.

`pagination_type: "scroll"` with no pattern is the one case that cannot work — infinite scroll needs a
real browser. Those sites get one page and a note in the report. If you can find the underlying
paged URL for them, putting it in `pagination_pattern` is enough to make them work; the type field
does not need to change.

Pagination also stops early when a page yields no links, repeats links already seen, hits
`--max-pages`/`--max-articles`, or (with `--since`) runs past the date cutoff.

### Speed

The scraper is fully async (tokio). Sites run in parallel (`--site-concurrency`), and within a site
the article fetches run concurrently (`--concurrency`) behind an even request pacing (`--delay-ms`).
While the current feed page's articles are being fetched, the next feed page is already downloading
in the background, so pagination never waits on parsing or scraping. HTTP connections are kept alive
and multiplexed over HTTP/2, with an adaptive flow-control window and a per-host connection pool.

### Logging

Every site logs one summary line when it finishes, and the run logs a totals line, both with counts
and percentages:

```text
done: pages 6/6 (100.0%), links 214 (37 dup, 17.3%), articles 82/89 (92.1%), failed 7, dated 81/82 (98.8%), titles 82 (100.0%), authors 64 (78.0%), internal links 1512 (18.4/art, 96.3% of articles)
```

The same numbers are in `run_summary.json`, `summary.json` per site, and the per-site table of
`run_summary.md` (saved, failed, dated, title and author columns all show counts with percentages;
`int.links` shows total links and links per article).

### How articles are found and parsed

`article_link_selector` often points at something that is not the `<a>` — the iz.ru config ends in a
`<span>`. For each match the scraper looks at the element's own `href`, then an enclosing `<a>`, then
an `<a>` inside it, then the first link in up to three enclosing containers. Links are made absolute,
de-duplicated, and restricted to the feed's own host.

When a selector matches nothing, it falls back to Open Graph / `<meta>` tags, JSON-LD
(`headline`, `articleBody`, `author`, `datePublished`), `<h1>`, `<time datetime>`. `field_sources`
records which one was used, so `run_summary.md` can tell you which of your selectors are dead weight.

Dates are parsed from ISO 8601, RFC 2822, `dd.mm.yyyy`, `dd/mm/yyyy`, English and Russian month
names, `N hours ago`, and dates embedded in longer strings. Values with no timezone are read as UTC,
which is accurate enough for day-level `--since`/`--until` filtering; the raw string is always kept.

Response bodies are decoded using the charset from the `Content-Type` header, then from the
document's `<meta charset>`, then UTF-8 — several of these sites are windows-1251.

### Reading the report

`run_summary.md` lists sites worst-first with a `health` score (half fetch success, half how many of
your configured selectors produced a value), then three sections that are the actionable part:

- **Sites that produced nothing** — with the reason: unreachable, no links matched, no `feed_link`.
- **Selectors that never matched** — configured in the JSON, matched nothing on any article. These are the ones to re-pick.
- **Selectors that would not parse as CSS** — typos in the selector string.

### Notes

- `robots.txt` is not consulted. `--delay-ms` and `--site-concurrency` are the politeness controls; keep them conservative on shared hosting.
- Requires a reasonably recent Rust (let-else syntax, so 1.65+).
- `cargo test` covers date parsing, pattern rendering and link/fallback extraction.
