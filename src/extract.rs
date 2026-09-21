//! All HTML parsing. Everything here is synchronous and returns owned data, so
//! callers can run it on `spawn_blocking` without holding a non-Send `Html`
//! across an await point.

use crate::config::SiteConfig;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

const IMAGE_ATTRS: &[&str] = &[
    "src",
    "data-src",
    "data-original",
    "data-lazy-src",
    "data-srcset",
    "srcset",
    "content",
];

const MEDIA_EXT: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".mp4", ".mp3", ".pdf", ".zip", ".avi",
];

// ---------------------------------------------------------------------------
// Feed pages
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct FeedPage {
    pub links: Vec<String>,
    pub next_url: Option<String>,
    pub bad_selectors: Vec<String>,
}

/// Collect article URLs from a feed page, plus the "next page" link if the
/// config provides a selector for it.
///
/// `article_link_selector` often points at something that is not the `<a>`
/// itself (a `<span>` with the headline, a card `<div>`), so for each match we
/// look at the element, then its ancestors, then its descendants.
pub fn parse_feed(html: &str, base_url: &str, article_sel: &str, next_sel: &str) -> FeedPage {
    let mut out = FeedPage::default();
    let mut bad: Vec<String> = Vec::new();
    let doc = Html::parse_document(html);
    let base = Url::parse(base_url).ok();
    let a_sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return out,
    };

    if let Some(sel) = parse_sel(article_sel, &mut bad) {
        let mut seen = Vec::new();
        for el in doc.select(&sel) {
            let Some(href) = href_for(el, &a_sel) else {
                continue;
            };
            let Some(abs) = absolutize(base.as_ref(), &href) else {
                continue;
            };
            if !same_site(base.as_ref(), &abs) || looks_like_media(&abs) {
                continue;
            }
            if !seen.contains(&abs) {
                seen.push(abs);
            }
        }
        out.links = seen;
    }

    if let Some(sel) = parse_sel(next_sel, &mut bad) {
        for el in doc.select(&sel) {
            let Some(href) = href_for(el, &a_sel) else {
                continue;
            };
            if let Some(abs) = absolutize(base.as_ref(), &href) {
                if Some(abs.as_str()) != base.as_ref().map(|b| b.as_str()) {
                    out.next_url = Some(abs);
                    break;
                }
            }
        }
    }

    out.bad_selectors = bad;
    out
}

/// Find the URL a matched element stands for, in order of confidence:
/// the element's own `href`, an enclosing `<a>`, an `<a>` inside it, and
/// finally the first `<a>` in one of the three enclosing containers (the
/// common "card with the headline in a span and the link next to it" shape).
fn href_for(el: ElementRef, a_sel: &Selector) -> Option<String> {
    if let Some(h) = el.value().attr("href") {
        return Some(h.to_string());
    }
    for ancestor in el.ancestors() {
        if let Some(ae) = ElementRef::wrap(ancestor) {
            if ae.value().name() == "a" {
                if let Some(h) = ae.value().attr("href") {
                    return Some(h.to_string());
                }
            }
        }
    }
    if let Some(h) = el
        .select(a_sel)
        .next()
        .and_then(|a| a.value().attr("href").map(|h| h.to_string()))
    {
        return Some(h);
    }
    let mut levels = 0;
    for ancestor in el.ancestors() {
        let Some(ae) = ElementRef::wrap(ancestor) else {
            continue;
        };
        if matches!(ae.value().name(), "body" | "html") {
            break;
        }
        levels += 1;
        if levels > 3 {
            break;
        }
        if let Some(a) = ae.select(a_sel).next() {
            if let Some(h) = a.value().attr("href") {
                return Some(h.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Article pages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ArticleSelectors {
    pub title: String,
    pub description: String,
    pub author: String,
    pub publish_date: String,
    pub images: String,
    pub comments: String,
    pub internal_links: String,
}

impl From<&SiteConfig> for ArticleSelectors {
    fn from(c: &SiteConfig) -> Self {
        Self {
            title: c.title_selector.clone(),
            description: c.description_selector.clone(),
            author: c.author_selector.clone(),
            publish_date: c.publish_date_selector.clone(),
            images: c.images_selector.clone(),
            comments: c.comments_selector.clone(),
            internal_links: c.internal_links_selector.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub publish_date_raw: Option<String>,
    pub images: Vec<String>,
    pub comments: Vec<String>,
    pub internal_links: Vec<String>,
    /// field -> where the value came from ("selector", "meta", "json-ld", ...).
    pub sources: BTreeMap<String, String>,
    pub bad_selectors: Vec<String>,
}

pub fn extract_article(html: &str, sels: &ArticleSelectors, url: &str) -> Extracted {
    let mut out = Extracted::default();
    let mut bad: Vec<String> = Vec::new();
    let doc = Html::parse_document(html);
    let base = Url::parse(url).ok();
    let ld = jsonld_blocks(&doc);

    // --- title -------------------------------------------------------------
    if let Some(v) = first_text(&doc, &sels.title, &mut bad) {
        out.set("title", v, "selector");
    } else if let Some(v) = meta_content(&doc, &["og:title", "twitter:title"]) {
        out.set("title", v, "meta");
    } else if let Some(v) = ld_string(&ld, "headline") {
        out.set("title", v, "json-ld");
    } else if let Some(v) = plain_text(&doc, "h1") {
        out.set("title", v, "h1");
    } else if let Some(v) = plain_text(&doc, "title") {
        out.set("title", v, "title-tag");
    }

    // --- description / body ------------------------------------------------
    if let Some(v) = joined_text(&doc, &sels.description, &mut bad) {
        out.set("description", v, "selector");
    } else if let Some(v) = ld_string(&ld, "articleBody") {
        out.set("description", v, "json-ld");
    } else if let Some(v) = meta_content(&doc, &["og:description", "description"]) {
        out.set("description", v, "meta");
    }

    // --- author ------------------------------------------------------------
    if let Some(v) = first_text(&doc, &sels.author, &mut bad) {
        out.set("author", v, "selector");
    } else if let Some(v) = meta_content(&doc, &["author", "article:author", "og:article:author"]) {
        out.set("author", v, "meta");
    } else if let Some(v) = ld_string(&ld, "author") {
        out.set("author", v, "json-ld");
    }

    // --- publish date ------------------------------------------------------
    if let Some(v) = date_from_selector(&doc, &sels.publish_date, &mut bad) {
        out.set("publish_date", v, "selector");
    } else if let Some(v) = meta_content(
        &doc,
        &[
            "article:published_time",
            "og:article:published_time",
            "datePublished",
            "pubdate",
            "date",
        ],
    ) {
        out.set("publish_date", v, "meta");
    } else if let Some(v) = ld_string(&ld, "datePublished") {
        out.set("publish_date", v, "json-ld");
    } else if let Some(v) = attr_of(&doc, "time[datetime]", "datetime") {
        out.set("publish_date", v, "time-tag");
    }

    // --- images ------------------------------------------------------------
    let imgs = url_attrs(&doc, &sels.images, IMAGE_ATTRS, base.as_ref(), &mut bad);
    if !imgs.is_empty() {
        out.images = imgs;
        out.sources.insert("images".into(), "selector".into());
    } else if let Some(v) = meta_content(&doc, &["og:image", "twitter:image"]) {
        if let Some(abs) = absolutize(base.as_ref(), &v) {
            out.images = vec![abs];
            out.sources.insert("images".into(), "meta".into());
        }
    }

    // --- comments ----------------------------------------------------------
    if let Some(sel) = parse_sel(&sels.comments, &mut bad) {
        out.comments = doc
            .select(&sel)
            .map(text_of)
            .filter(|s| !s.is_empty())
            .collect();
        if !out.comments.is_empty() {
            out.sources.insert("comments".into(), "selector".into());
        }
    }

    // --- internal links ----------------------------------------------------
    if let Some(sel) = parse_sel(&sels.internal_links, &mut bad) {
        let a_sel = Selector::parse("a[href]").ok();
        let mut links: Vec<String> = Vec::new();
        for el in doc.select(&sel) {
            let href = match a_sel.as_ref() {
                Some(a) => href_for(el, a),
                None => el.value().attr("href").map(|h| h.to_string()),
            };
            let Some(href) = href else { continue };
            let Some(abs) = absolutize(base.as_ref(), &href) else {
                continue;
            };
            if same_site(base.as_ref(), &abs) && !looks_like_media(&abs) && !links.contains(&abs) {
                links.push(abs);
            }
        }
        if !links.is_empty() {
            out.internal_links = links;
            out.sources
                .insert("internal_links".into(), "selector".into());
        }
    }

    out.bad_selectors = bad;
    out
}

impl Extracted {
    fn set(&mut self, field: &str, value: String, source: &str) {
        let value = value.trim().to_string();
        if value.is_empty() {
            return;
        }
        match field {
            "title" => self.title = Some(value),
            "description" => self.description = Some(value),
            "author" => self.author = Some(value),
            "publish_date" => self.publish_date_raw = Some(value),
            _ => return,
        }
        self.sources.insert(field.to_string(), source.to_string());
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn parse_sel(raw: &str, bad: &mut Vec<String>) -> Option<Selector> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match Selector::parse(raw) {
        Ok(s) => Some(s),
        Err(e) => {
            bad.push(format!("{raw}  ->  {e:?}"));
            None
        }
    }
}

/// Text of an element with `<script>`/`<style>` subtrees skipped and block
/// elements turned into line breaks.
fn text_of(el: ElementRef) -> String {
    let mut buf = String::new();
    collect_text(el, &mut buf);
    normalize(&buf)
}

fn collect_text(el: ElementRef, out: &mut String) {
    for child in el.children() {
        if let Some(t) = child.value().as_text() {
            out.push_str(&t.text);
        } else if let Some(ce) = ElementRef::wrap(child) {
            let name = ce.value().name();
            if matches!(
                name,
                "script" | "style" | "noscript" | "svg" | "iframe" | "template"
            ) {
                continue;
            }
            let block = matches!(
                name,
                "p" | "div"
                    | "br"
                    | "li"
                    | "tr"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "section"
                    | "blockquote"
            );
            if block {
                out.push('\n');
            }
            collect_text(ce, out);
            if block {
                out.push('\n');
            }
        }
    }
}

fn normalize(s: &str) -> String {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_text(doc: &Html, sel: &str, bad: &mut Vec<String>) -> Option<String> {
    let sel = parse_sel(sel, bad)?;
    doc.select(&sel).map(text_of).find(|s| !s.is_empty())
}

/// All matches joined - article bodies are usually `div > p` with many matches.
fn joined_text(doc: &Html, sel: &str, bad: &mut Vec<String>) -> Option<String> {
    let sel = parse_sel(sel, bad)?;
    let parts: Vec<String> = doc
        .select(&sel)
        .map(text_of)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn plain_text(doc: &Html, sel: &str) -> Option<String> {
    let sel = Selector::parse(sel).ok()?;
    doc.select(&sel).map(text_of).find(|s| !s.is_empty())
}

fn attr_of(doc: &Html, sel: &str, attr: &str) -> Option<String> {
    let sel = Selector::parse(sel).ok()?;
    doc.select(&sel)
        .find_map(|e| e.value().attr(attr).map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
}

/// A date can live in `datetime`/`content`/`title` before it lives in the text.
fn date_from_selector(doc: &Html, sel: &str, bad: &mut Vec<String>) -> Option<String> {
    let sel = parse_sel(sel, bad)?;
    for el in doc.select(&sel) {
        for attr in ["datetime", "content", "data-time", "title"] {
            if let Some(v) = el.value().attr(attr) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        let t = text_of(el);
        if !t.is_empty() {
            return Some(t.replace('\n', " "));
        }
    }
    None
}

fn meta_content(doc: &Html, keys: &[&str]) -> Option<String> {
    for key in keys {
        for pattern in [
            format!(r#"meta[property="{key}"]"#),
            format!(r#"meta[name="{key}"]"#),
            format!(r#"meta[itemprop="{key}"]"#),
        ] {
            if let Some(v) = attr_of(doc, &pattern, "content") {
                return Some(v);
            }
        }
    }
    None
}

fn url_attrs(
    doc: &Html,
    sel: &str,
    attrs: &[&str],
    base: Option<&Url>,
    bad: &mut Vec<String>,
) -> Vec<String> {
    let Some(sel) = parse_sel(sel, bad) else {
        return Vec::new();
    };
    let img_sel = Selector::parse("img").ok();
    let mut out: Vec<String> = Vec::new();

    for el in doc.select(&sel) {
        let mut raw = attrs.iter().find_map(|a| el.value().attr(*a));
        if raw.is_none() {
            if let Some(is) = img_sel.as_ref() {
                if let Some(inner) = el.select(is).next() {
                    raw = attrs.iter().find_map(|a| inner.value().attr(*a));
                }
            }
        }
        let Some(raw) = raw else { continue };
        // srcset is "url 1x, url 2x" - take the first URL.
        let first = raw.split(',').next().unwrap_or(raw);
        let first = first.split_whitespace().next().unwrap_or(first);
        if let Some(abs) = absolutize(base, first) {
            if !out.contains(&abs) {
                out.push(abs);
            }
        }
    }
    out
}

fn absolutize(base: Option<&Url>, href: &str) -> Option<String> {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') {
        return None;
    }
    let low = h.to_ascii_lowercase();
    if low.starts_with("javascript:")
        || low.starts_with("mailto:")
        || low.starts_with("tel:")
        || low.starts_with("data:")
    {
        return None;
    }
    let mut u = match Url::parse(h) {
        Ok(u) => u,
        Err(_) => base?.join(h).ok()?,
    };
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    u.set_fragment(None);
    Some(u.to_string())
}

fn same_site(base: Option<&Url>, candidate: &str) -> bool {
    let Some(base) = base else { return true };
    let (Some(bh), Ok(cu)) = (base.host_str(), Url::parse(candidate)) else {
        return true;
    };
    let Some(ch) = cu.host_str() else {
        return false;
    };
    let bh = bh.trim_start_matches("www.");
    let ch = ch.trim_start_matches("www.");
    ch == bh || ch.ends_with(&format!(".{bh}")) || bh.ends_with(&format!(".{ch}"))
}

fn looks_like_media(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    MEDIA_EXT.iter().any(|e| path.ends_with(e))
}

// ---------------------------------------------------------------------------
// JSON-LD
// ---------------------------------------------------------------------------

fn jsonld_blocks(doc: &Html) -> Vec<Value> {
    let Ok(sel) = Selector::parse(r#"script[type="application/ld+json"]"#) else {
        return Vec::new();
    };
    doc.select(&sel)
        .filter_map(|e| {
            let raw: String = e.text().collect();
            serde_json::from_str::<Value>(raw.trim()).ok()
        })
        .collect()
}

fn ld_string(blocks: &[Value], key: &str) -> Option<String> {
    blocks.iter().find_map(|v| walk(v, key))
}

fn walk(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(scalarize) {
                return Some(found);
            }
            map.values().find_map(|child| walk(child, key))
        }
        Value::Array(items) => items.iter().find_map(|child| walk(child, key)),
        _ => None,
    }
}

/// `"author"` may be a string, `{"name": ...}` or a list of either.
fn scalarize(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(map) => map.get("name").and_then(scalarize),
        Value::Array(items) => items.iter().find_map(scalarize),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"
      <html><body>
        <div class="card"><div class="info"><span>Headline one</span></div>
          <a href="/news/1">read</a></div>
        <a href="/news/2"><span class="t">Headline two</span></a>
        <a class="more" href="/news?page=2">next</a>
      </body></html>"#;

    #[test]
    fn finds_links_through_ancestors_and_descendants() {
        let f = parse_feed(FEED, "https://ex.com/news", "span", "a.more");
        assert!(f.links.iter().any(|l| l.ends_with("/news/1")));
        assert!(f.links.iter().any(|l| l.ends_with("/news/2")));
        assert_eq!(f.next_url.as_deref(), Some("https://ex.com/news?page=2"));
    }

    #[test]
    fn falls_back_to_meta_tags() {
        let html = r#"<html><head>
            <meta property="og:title" content="Meta title">
            <meta property="article:published_time" content="2026-09-18T10:00:00Z">
          </head><body><div class="b"><p>Body text.</p><script>junk()</script></div></body></html>"#;
        let sels = ArticleSelectors {
            title: "h1.missing".into(),
            description: "div.b".into(),
            author: String::new(),
            publish_date: String::new(),
            images: String::new(),
            comments: String::new(),
            internal_links: String::new(),
        };
        let e = extract_article(html, &sels, "https://ex.com/a");
        assert_eq!(e.title.as_deref(), Some("Meta title"));
        assert_eq!(e.sources.get("title").map(String::as_str), Some("meta"));
        assert_eq!(e.description.as_deref(), Some("Body text."));
        assert_eq!(e.publish_date_raw.as_deref(), Some("2026-09-18T10:00:00Z"));
    }
}
