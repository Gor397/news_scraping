# News scraper

Four pieces:

1. `scraperBookmark.js` — a bookmarklet that builds a site's selector JSON by clicking elements on the page.
2. `merge_selectors.py` — folds the split selector files (`site.json` + `site(1).json`) into one file per site.
3. `news-scraper` — a Rust CLI that walks each site's feed, paginates, scrapes the articles and reports how well it did. Saves to JSON files or a Postgres database.
4. `override_selectors.py` — folds improved selector files over merged ones; empty fields keep the original.
5. `upload_selectors.py` — uploads merged selector files into the Postgres `site_configs` table.

## Running inside the Django project

The scraper is integrated into the Django backend via `scrape_rust`, which runs
the binary in `--db` mode against the project's Postgres and then imports the
scraped rows from the `articles` table into the Django `Post` table (through
`PostService.save_many`, so journalists, images, links and the alerting
pipeline all behave exactly like the other scrapers).

```bash
make scrape_rust                                    # full run: scrape + import
make scrape_rust ARGS="--site example.com --max-pages 5"
make scrape_rust ARGS="--list-sites"                # dry plan, no requests
make scrape_rust ARGS="--sync-selectors-only"       # just upload selectors_merged
make scrape_rust ARGS="--import-only"               # import pending rows only
make scrape_rust ARGS="--no-import"                 # scrape, leave Post alone
make scrape_rust ARGS="--reset-cursor --import-only"  # re-import everything
```

Implementation: `news_classification/services/rust_scraper_import.py` and
`news_classification/management/commands/scrape_rust.py`. Import is
incremental — a cursor (`last_article_id`) stored on the run's `ScrapingRun`
row keeps track of what has already been imported, so re-running only pulls
new articles. Sites that have no matching `NewsWebsite` row are imported with
a NULL website reference.

## Layout

A typical working directory:

```
.
├── scraperBookmark.js      <- bookmarklet source, pasted into a browser bookmark
├── selectors/              <- raw selector files, one or more per site
├── selectors_merged/       <- created by step 2 (merged multiple json files for the same website)
├── merge_selectors.py      <- script for merging 2 or more json files of the same webste into 1 json file and adding metadata
├── new_selectors/          <- bookmarklet downloads land here (step 1) (new partially updated css selectors for overriding the old selectors)
├── new_selectors_merged/   <- created by running step 2 on them (merged new updated selectors)
├── override_selectors.py   <- override the old json files with the new updated css selectors from new_selectors_merged folder
├── upload_selectors.py     <- upload css selector json files to postegres db
├── Cargo.toml              <- the Rust scraper (step 3)
├── src/                    <- scraper
└── output/                 <- created by step 3 (file mode)
```

## 1. Pick selectors with the bookmarklet

`scraperBookmark.js` is a bookmarklet that builds a site's selector JSON by clicking elements on the
page instead of digging through HTML in devtools.

**Install it once:** create a new bookmark in your browser (name it e.g. `Scraper Setup`) and paste
the entire contents of `scraperBookmark.js` — the whole `javascript:(function(){...})();` line — into
the bookmark's URL field.

**Use it per site:**

1. Open the site's feed page and click the bookmark. A "Scraper Setup" panel opens in the top-right
   with all 13 fields; `website_link` is filled in automatically from the page's origin.
2. Click a field in the panel to make it active, then click the matching element on the page
   (hovering outlines elements in blue). The panel records a short CSS path for the click — up to
   three levels of tag/`#id`/classes — and jumps to the next empty field.
3. A few fields are typed rather than clicked:
   - `feed_link` — press **Capture Current** while on the feed page, or click the row and paste a URL.
   - `pagination_type` — a prompt: `1` Infinite Scroll (`scroll`), `2` Next Button (`next_button`),
     `3` Page Numbers (`page_numbers`), `4` URL Pattern (`url_pattern`).
   - `pagination_pattern` (e.g. `https://site.com/news?page={page}`) and `first_page_number` — text prompts.
4. The article-side fields (`title_selector`, `description_selector`, …) are picked on an article
   page: switch to **Mode: Navigating**, open any article as a normal visitor, switch back to
   **Mode: Selecting** and click the title, date, and so on.
5. **Download JSON** saves `<hostname>.json`; **Close** closes the panel and wipes the saved state.

Progress is kept in `sessionStorage`, so reloads and navigating between pages don't lose your picks —
but it is per tab, so do the whole site in one tab. If you do the feed side and article side in two
separate sessions you end up with `host.json` and `host(1).json`, which is exactly the split step 2
merges.

Put the downloads in a folder and merge them the same way as step 2:

```bash
python merge_selectors.py -i new_selectors -o new_selectors_merged
```

## 2. Merge

```bash
python merge_selectors.py -i selectors -o selectors_merged
```

Files are grouped by the host in `website_link` (not by file name), so `example.com.json` and
`example.com(1).json` land in the same group regardless of the `(N)` convention. Within a group each
field takes the first non-empty value, base file before `(1)` before `(2)`.

The output keeps the original 14 fields and adds two:

- `extra_feed_links` — if two source files disagreed on `feed_link`, the site has more than one feed; the scraper walks all of them.
- `_meta` — source files, conflicting values, and which required fields are missing. The scraper ignores it.

It prints (and writes to `selectors_merged/_merge_report.json`) which sites were assembled from
several files, where values conflicted, and which sites are unusable because `feed_link` or
`article_link_selector` is empty. Add `--check` to analyse without writing.

## 3. Scrape

Needs Rust: <https://rustup.rs>. Then:

```bash
cargo build --release
```

The binary lands at `target/release/news-scraper` (`.exe` on Windows). Run it from the folder that
holds `selectors_merged`.

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
news-scraper --since 2025-01-01 --max-pages 25
```

One site, verbose, one JSON file per article:

```bash
news-scraper --site example.com --format files -v
```

### Options

| flag | meaning |
|---|---|
| `--config-dir <dir>` | merged selector files (default `selectors_merged`); optional with `--db` |
| `--out <dir>` | output root (default `output`) |
| `--db <url>` | Postgres connection URL: read configs from the DB and save articles there instead of to files |
| `--db-init` | create the DB tables, then exit; combine with `--db` |
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

By default articles land in the `output/` tree:

```
output/
├── run_summary.json          machine-readable, every site
├── run_summary.md            the readable one - start here
└── example.com/
    ├── articles.jsonl        one article per line
    ├── _seen_urls.txt        used by --resume
    └── summary.json          this site's report
```

With `--db` the articles go to Postgres instead (see below); the two `run_summary` files are still
written under `--out`.

### Postgres instead of files

Pass `--db` with a connection URL and the scraper changes both ends: site configs are read from the
`site_configs` table and every scraped article is stored in the database instead of the `output/`
tree. `--format`, `--out` (for articles) and `--resume`'s `_seen_urls.txt` play no role in this mode.

One-time setup:

```bash
# create the tables
news-scraper --db "postgresql://user:pass@localhost:5432/news" --db-init

# upload the merged selector files
python upload_selectors.py -i selectors_merged -d "postgresql://user:pass@localhost:5432/news"
```

Then run without any JSON folder:

```bash
news-scraper --db "postgresql://user:pass@localhost:5432/news"
```

`upload_selectors.py` upserts one row per file (`<name>.json` -> host `name`, the file's content as
JSONB), so re-uploading an improved file updates that site only; add `--replace` to wipe the table
first. It needs `pip install psycopg2-binary` and takes `-d` or the `DATABASE_URL` env var.

`--db` and `--config-dir` can be combined: configs are then read from the table *and* the folder
(the scraper de-duplicates nothing, so a host present in both is scraped twice - pick one source).

Tables the scraper uses:

| table | contents |
|---|---|
| `site_configs` | one JSONB config per host, as uploaded by `upload_selectors.py` |
| `articles` | one row per article: title, author, description, dates, `field_sources`, ...; unique on `(site, url)` |
| `article_images` / `article_links` | the images, comments (`kind = 'comment'`) and internal links (`kind = 'internal'`) per article |
| `seen_urls` | what has already been saved, per site - the DB equivalent of `--resume`, always on |

Re-running with the same `--db` skips URLs already in `seen_urls`, like `--resume` does for files.

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

`article_link_selector` often points at something that is not the `<a>` — a `<span>` inside the link,
for example. For each match the scraper looks at the element's own `href`, then an enclosing `<a>`, then
an `<a>` inside it, then the first link in up to three enclosing containers. Links are made absolute,
de-duplicated, and restricted to the feed's own host.

When a selector matches nothing, it falls back to Open Graph / `<meta>` tags, JSON-LD
(`headline`, `articleBody`, `author`, `datePublished`), `<h1>`, `<time datetime>`. `field_sources`
records which one was used, so `run_summary.md` can tell you which of your selectors are dead weight.

Dates are parsed from ISO 8601, RFC 2822, `dd.mm.yyyy`, `dd/mm/yyyy`, English and Russian month
names, `N hours ago`, and dates embedded in longer strings. Values with no timezone are read as UTC,
which is accurate enough for day-level `--since`/`--until` filtering; the raw string is always kept.

Response bodies are decoded using the charset from the `Content-Type` header, then from the
document's `<meta charset>`, then UTF-8 — some sites serve legacy encodings such as windows-1251.

### Reading the report

`run_summary.md` lists sites worst-first with a `health` score (half fetch success, half how many of
your configured selectors produced a value), then three sections that are the actionable part:

- **Sites that produced nothing** — with the reason: unreachable, no links matched, no `feed_link`.
- **Selectors that never matched** — configured in the JSON, matched nothing on any article. These are the ones to re-pick.
- **Selectors that would not parse as CSS** — typos in the selector string.

Re-pick the failing ones with the bookmarklet (step 1) and fold the fixes in as described in step 4.

## 4. Override selectors with improved ones

The files the bookmarklet produces are complete, but you usually only want to replace the fields that
were actually wrong. `override_selectors.py` folds the new files over the merged ones:

```bash
python override_selectors.py \
    --base selectors_merged \
    --override new_selectors_merged \
    --out selectors_final
```

- Files are matched by name: `example.com.json` in the override folder updates `example.com.json` in the
  base folder.
- Only non-empty values override. `""`, `[]`, `{}` and `null` all mean "keep the original", so a
  bookmarklet file that only fixed `title_selector` touches nothing else. Nested objects like
  `_meta` are merged recursively with the same rule.
- Files with no matching base file are skipped (and reported); base files with no override are
  copied through unchanged, so `--out` is always a complete set you can point the scraper at:

  ```bash
  news-scraper --config-dir selectors_final
  ```

Omit `--out` and the base folder is overwritten in place, which keeps the scraper's default
`--config-dir` working. With no arguments at all it does exactly that: `--base selectors_merged`,
`--override new_selectors_merged`, merged in place.

### Notes

- `robots.txt` is not consulted. `--delay-ms` and `--site-concurrency` are the politeness controls; keep them conservative on shared hosting.
- Requires a reasonably recent Rust (let-else syntax, so 1.65+).
- `cargo test` covers date parsing, pattern rendering, link/fallback extraction and the DB host sanitising.
- Postgres connections use TLS when the server offers it; the URL goes in `--db` (or `DATABASE_URL` for `upload_selectors.py`) and is not written into the summary files.
