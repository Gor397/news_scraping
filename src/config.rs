//! Loading of the merged selector files produced by `merge_selectors.py`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

/// Accepts a string, a number or null and always yields a (trimmed) String.
/// The selector files are hand-made, so `"first_page_number": 1` happens.
fn flex_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a string, number or null")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.trim().to_string())
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v.trim().to_string())
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_bool<E: de::Error>(self, _v: bool) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_none<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_some<D2>(self, d: D2) -> Result<String, D2::Error>
        where
            D2: serde::Deserializer<'de>,
        {
            d.deserialize_any(V)
        }
    }

    d.deserialize_any(V)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SiteConfig {
    #[serde(default, deserialize_with = "flex_string")]
    pub website_link: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub feed_link: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub article_link_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub pagination_type: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub pagination_pattern: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub next_page_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub first_page_number: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub title_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub description_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub author_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub publish_date_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub images_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub comments_selector: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub internal_links_selector: String,

    /// Written by the merge script when two source files disagreed on `feed_link`.
    #[serde(default)]
    pub extra_feed_links: Vec<String>,

    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub source_file: PathBuf,
}

impl SiteConfig {
    /// Every feed page to start from: the primary one plus any extras.
    pub fn feeds(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.feed_link.is_empty() {
            out.push(self.feed_link.clone());
        }
        for f in &self.extra_feed_links {
            let f = f.trim();
            if !f.is_empty() && !out.iter().any(|x| x == f) {
                out.push(f.to_string());
            }
        }
        out
    }

    /// Selectors that were left blank in the config, for the report.
    pub fn unconfigured(&self) -> Vec<&'static str> {
        let pairs: [(&'static str, &str); 7] = [
            ("title", self.title_selector.as_str()),
            ("description", self.description_selector.as_str()),
            ("author", self.author_selector.as_str()),
            ("publish_date", self.publish_date_selector.as_str()),
            ("images", self.images_selector.as_str()),
            ("comments", self.comments_selector.as_str()),
            ("internal_links", self.internal_links_selector.as_str()),
        ];
        pairs
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(k, _)| *k)
            .collect()
    }
}

/// Load every `*.json` in `dir`, skipping files whose name starts with `_`
/// (that is where the merge script puts its own report).
pub fn load_dir(dir: &Path) -> Result<Vec<SiteConfig>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read config dir {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("json")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('_'))
                    .unwrap_or(true)
        })
        .collect();
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let text = text.trim_start_matches('\u{feff}');
        let mut cfg: SiteConfig = serde_json::from_str(text)
            .with_context(|| format!("cannot parse {}", path.display()))?;
        cfg.name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        cfg.source_file = path;
        out.push(cfg);
    }
    Ok(out)
}
