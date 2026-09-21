//! Turning the four pagination fields into something we can actually walk.
//!
//! `pagination_type` is a hint, not an instruction: what matters is whether a
//! usable `pagination_pattern` or `next_page_selector` exists. A pattern always
//! wins because it needs one request per page instead of a chain, and it is the
//! only thing that lets us page an infinite-scroll feed without a browser.

use crate::config::SiteConfig;
use regex::Regex;
use url::Url;

#[derive(Debug, Clone)]
pub enum Strategy {
    /// `site.com/news/page/{page}` - any `{placeholder}` is replaced.
    UrlPattern { pattern: String },
    /// Follow the href of whatever `next_page_selector` matches.
    NextButton { selector: String },
    /// Feed page only.
    Single,
}

impl Strategy {
    pub fn label(&self) -> &'static str {
        match self {
            Strategy::UrlPattern { .. } => "url_pattern",
            Strategy::NextButton { .. } => "next_button",
            Strategy::Single => "single_page",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub strategy: Strategy,
    /// Page number the pattern starts at (`first_page_number`, default 1).
    pub start: i64,
    pub reason: String,
    pub notes: Vec<String>,
}

pub fn plan(cfg: &SiteConfig) -> Plan {
    let kind = cfg.pagination_type.to_ascii_lowercase();
    let pattern = cfg.pagination_pattern.trim().to_string();
    let next = cfg.next_page_selector.trim().to_string();
    let start = cfg.first_page_number.trim().parse::<i64>().unwrap_or(1);
    let mut notes = Vec::new();

    if !pattern.is_empty() {
        if !has_placeholder(&pattern) {
            notes.push(format!(
                "pagination_pattern '{pattern}' has no {{placeholder}}; every page would be the same URL"
            ));
        } else {
            return Plan {
                strategy: Strategy::UrlPattern {
                    pattern: pattern.clone(),
                },
                start,
                reason: format!("pagination_pattern from page {start}"),
                notes,
            };
        }
    }

    if !next.is_empty() {
        return Plan {
            strategy: Strategy::NextButton {
                selector: next.clone(),
            },
            start,
            reason: format!("next_page_selector '{next}'"),
            notes,
        };
    }

    match kind.as_str() {
        "scroll" => notes.push(
            "pagination_type=scroll with no pagination_pattern: infinite scroll needs a browser, \
             so only the first feed page is read"
                .to_string(),
        ),
        "page_numbers" | "url_pattern" => notes.push(format!(
            "pagination_type={kind} but neither pagination_pattern nor next_page_selector is set; \
             only the first feed page is read"
        )),
        "next_button" => notes.push(
            "pagination_type=next_button but next_page_selector is empty; only the first feed page \
             is read"
                .to_string(),
        ),
        _ => notes.push("no usable pagination config; only the first feed page is read".to_string()),
    }

    Plan {
        strategy: Strategy::Single,
        start,
        reason: "no pattern and no next-page selector".to_string(),
        notes,
    }
}

fn has_placeholder(pattern: &str) -> bool {
    placeholder_re().is_match(pattern)
}

fn placeholder_re() -> Regex {
    // Built per call; this runs once per site, not per page.
    Regex::new(r"\{[A-Za-z0-9_]*\}").expect("static regex")
}

/// Substitute the page number and make the result absolute.
pub fn render(pattern: &str, page: i64, base: &str) -> String {
    let re = placeholder_re();
    let filled = re
        .replace_all(pattern, page.to_string().as_str())
        .to_string();
    if filled.starts_with("http://") || filled.starts_with("https://") {
        return filled;
    }
    match Url::parse(base).and_then(|b| b.join(&filled)) {
        Ok(u) => u.to_string(),
        Err(_) => filled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_placeholders() {
        assert_eq!(
            render("https://s.com/news/page/{page}", 3, "https://s.com"),
            "https://s.com/news/page/3"
        );
        assert_eq!(
            render("/news?p={n}", 2, "https://s.com/news"),
            "https://s.com/news?p=2"
        );
        assert_eq!(
            render("https://s.com/a/{}/b", 5, "https://s.com"),
            "https://s.com/a/5/b"
        );
    }
}
