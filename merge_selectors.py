#!/usr/bin/env python3
"""
Merge split selector files into one JSON per news site.

Some sites are described by two (or more) files:

    en.irna.ir.json      -> feed side  (feed_link, article_link_selector, pagination...)
    en.irna.ir(1).json   -> article side (title_selector, description_selector, ...)

Files are grouped by the host of `website_link` (falling back to the file name
with any trailing "(N)" stripped). Within a group, each field takes the first
non-empty value, scanning files in order: base file first, then (1), (2), ...

Usage:
    python merge_selectors.py                       # ./selectors -> ./selectors_merged
    python merge_selectors.py -i selectors -o merged
    python merge_selectors.py --check               # report only, write nothing
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import OrderedDict, defaultdict
from pathlib import Path
from urllib.parse import urlparse

FIELDS = [
    "website_link",
    "feed_link",
    "article_link_selector",
    "pagination_type",
    "pagination_pattern",
    "next_page_selector",
    "first_page_number",
    "title_selector",
    "description_selector",
    "author_selector",
    "publish_date_selector",
    "images_selector",
    "comments_selector",
    "internal_links_selector",
]

# Fields that describe the feed page vs. the article page. Used only for the
# human-readable report ("this site has no article-side selectors at all").
FEED_FIELDS = {
    "feed_link",
    "article_link_selector",
    "pagination_type",
    "pagination_pattern",
    "next_page_selector",
    "first_page_number",
}
ARTICLE_FIELDS = {
    "title_selector",
    "description_selector",
    "author_selector",
    "publish_date_selector",
    "images_selector",
    "comments_selector",
    "internal_links_selector",
}

DUP_SUFFIX = re.compile(r"\s*\((\d+)\)\s*$")


def split_stem(stem: str):
    """'en.irna.ir(1)' -> ('en.irna.ir', 1);  'iz.ru' -> ('iz.ru', 0)"""
    m = DUP_SUFFIX.search(stem)
    if m:
        return stem[: m.start()].strip(), int(m.group(1))
    return stem.strip(), 0


def as_text(value) -> str:
    """Normalise any JSON scalar to a trimmed string; None/{}/[] become ''."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    if isinstance(value, bool):
        return ""
    if isinstance(value, (int, float)):
        return str(value)
    return ""


def site_key(data: dict, stem_base: str) -> str:
    host = urlparse(as_text(data.get("website_link"))).hostname
    return (host or stem_base).lower().strip()


def load(path: Path):
    try:
        with path.open("r", encoding="utf-8-sig") as fh:
            data = json.load(fh)
    except Exception as exc:  # noqa: BLE001 - we want the file name in the message
        return None, f"{path.name}: unreadable ({exc})"
    if not isinstance(data, dict):
        return None, f"{path.name}: top level is {type(data).__name__}, expected object"
    return data, None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-i", "--input", default="selectors", type=Path, help="folder with the raw selector files")
    ap.add_argument("-o", "--output", default="selectors_merged", type=Path, help="folder to write merged files to")
    ap.add_argument("--check", action="store_true", help="analyse and report, but write nothing")
    args = ap.parse_args()

    in_dir: Path = args.input
    if not in_dir.is_dir():
        print(f"error: input folder not found: {in_dir.resolve()}", file=sys.stderr)
        return 2

    files = sorted(p for p in in_dir.glob("*.json") if not p.name.startswith("_"))
    if not files:
        print(f"error: no .json files in {in_dir.resolve()}", file=sys.stderr)
        return 2

    problems: list[str] = []
    groups: dict[str, list] = defaultdict(list)

    for path in files:
        data, err = load(path)
        if err:
            problems.append(err)
            continue
        base, order = split_stem(path.stem)
        groups[site_key(data, base)].append((order, path.name, data))

    merged_all = OrderedDict()
    report = OrderedDict()

    for key in sorted(groups):
        parts = sorted(groups[key], key=lambda t: (t[0], t[1]))
        merged = OrderedDict((f, "") for f in FIELDS)
        provenance: dict[str, str] = {}
        conflicts: dict[str, list] = defaultdict(list)
        extra_feeds: list[str] = []

        for _order, fname, data in parts:
            for field in FIELDS:
                value = as_text(data.get(field))
                if not value:
                    continue
                if not merged[field]:
                    merged[field] = value
                    provenance[field] = fname
                elif merged[field] != value:
                    conflicts[field].append({"file": fname, "value": value})
                    if field == "feed_link" and value not in extra_feeds:
                        extra_feeds.append(value)

            unknown = [k for k in data if k not in FIELDS and not k.startswith("_")]
            for k in unknown:
                problems.append(f"{fname}: unknown field '{k}' ignored")

        if not merged["website_link"]:
            merged["website_link"] = f"https://{key}"
            provenance["website_link"] = "(derived from file name)"

        missing_feed = sorted(f for f in FEED_FIELDS if f in ("feed_link", "article_link_selector") and not merged[f])
        missing_article = sorted(f for f in ("title_selector", "description_selector") if not merged[f])

        merged["extra_feed_links"] = extra_feeds
        merged["_meta"] = OrderedDict(
            [
                ("site", key),
                ("sources", [fname for _o, fname, _d in parts]),
                ("conflicts", {k: v for k, v in conflicts.items()}),
                ("missing_required", missing_feed),
                ("missing_article_core", missing_article),
            ]
        )

        merged_all[key] = merged
        report[key] = {
            "sources": [fname for _o, fname, _d in parts],
            "filled": sum(1 for f in FIELDS if merged[f]),
            "conflicts": sorted(conflicts),
            "missing_required": missing_feed,
            "missing_article_core": missing_article,
            "extra_feed_links": extra_feeds,
        }

    # ---- write -------------------------------------------------------------
    if not args.check:
        out_dir: Path = args.output
        out_dir.mkdir(parents=True, exist_ok=True)
        for key, merged in merged_all.items():
            safe = re.sub(r"[^A-Za-z0-9._-]", "_", key)
            with (out_dir / f"{safe}.json").open("w", encoding="utf-8") as fh:
                json.dump(merged, fh, ensure_ascii=False, indent=2)
                fh.write("\n")
        with (out_dir / "_merge_report.json").open("w", encoding="utf-8") as fh:
            json.dump(
                {"input": str(in_dir), "files_read": len(files), "sites": report, "problems": problems},
                fh,
                ensure_ascii=False,
                indent=2,
            )
            fh.write("\n")

    # ---- report ------------------------------------------------------------
    multi = {k: v for k, v in report.items() if len(v["sources"]) > 1}
    conflicted = {k: v for k, v in report.items() if v["conflicts"]}
    broken = {k: v for k, v in report.items() if v["missing_required"]}
    thin = {k: v for k, v in report.items() if v["missing_article_core"]}

    print(f"read    : {len(files)} file(s) from {in_dir.resolve()}")
    print(f"merged  : {len(merged_all)} site(s)")
    print(f"combined: {len(multi)} site(s) built from more than one file")
    if not args.check:
        print(f"wrote   : {args.output.resolve()}")

    if multi:
        print("\nsites assembled from multiple files:")
        for k, v in sorted(multi.items()):
            print(f"  {k:<32} {' + '.join(v['sources'])}")

    if conflicted:
        print("\nconflicting values (first one kept, see _merge_report.json):")
        for k, v in sorted(conflicted.items()):
            print(f"  {k:<32} {', '.join(v['conflicts'])}")

    if broken:
        print("\nunusable - missing feed_link and/or article_link_selector:")
        for k, v in sorted(broken.items()):
            print(f"  {k:<32} missing {', '.join(v['missing_required'])}")

    if thin:
        print("\nno title/description selector (scraper will fall back to meta tags):")
        for k, v in sorted(thin.items()):
            print(f"  {k:<32} missing {', '.join(v['missing_article_core'])}")

    if problems:
        print(f"\n{len(problems)} problem(s):")
        for p in problems:
            print(f"  {p}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
