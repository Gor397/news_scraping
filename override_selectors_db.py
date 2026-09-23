#!/usr/bin/env python3
"""
Update existing selector configs in Postgres, field by field.

Unlike upload_selectors.py (which overwrites a host's whole `config` blob),
this script merges: for each <name>.json, only keys with a non-empty value
overwrite the corresponding key already stored in `site_configs.config` for
that host. Keys that are empty/null/missing in the file leave the existing
DB value untouched. Hosts with no existing row are inserted as-is (empty
keys included, since there's nothing to preserve).

"Empty" means: None, "", [], {}, or a string that is only whitespace.
Everything else (including 0, false, "0") counts as a real value.

Requires the schema from `news-scraper --db-init` (see db.rs SCHEMA).

Examples:
    python update_selectors.py -d postgresql://user:pass@localhost:5432/news
    python update_selectors.py -i selectors_merged -d $DATABASE_URL
    python update_selectors.py -i selectors_merged -d $DATABASE_URL --dry-run
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

SELECT_ONE = "SELECT config FROM site_configs WHERE host = %s"

UPSERT = """
INSERT INTO site_configs (host, config, updated_at)
VALUES (%s, %s, now())
ON CONFLICT (host) DO UPDATE
   SET config = EXCLUDED.config,
       updated_at = now()
"""


def _is_empty(value: Any) -> bool:
    if value is None:
        return True
    if isinstance(value, str):
        return value.strip() == ""
    if isinstance(value, (list, dict)):
        return len(value) == 0
    return False


def merge_configs(old: dict, new: dict) -> tuple[dict, list[str]]:
    """Return (merged, changed_keys). `old` may be {} if there was no row."""
    merged = dict(old)
    changed = []
    for key, new_val in new.items():
        if _is_empty(new_val):
            continue  # keep whatever is already in `merged` (old value, or nothing)
        old_val = old.get(key)
        if old_val != new_val:
            changed.append(key)
        merged[key] = new_val
    return merged, changed


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "-i",
        "--input",
        default="selectors_merged",
        type=Path,
        help="folder with selector files to apply as updates (default: selectors_merged)",
    )
    ap.add_argument(
        "-d",
        "--database",
        default=None,
        help="postgres URL, e.g. postgresql://user:pass@localhost:5432/news "
        "(default: DATABASE_URL env var)",
    )
    ap.add_argument(
        "--dry-run",
        action="store_true",
        help="show what would change per host, but write nothing",
    )
    args = ap.parse_args()

    dsn = args.database
    if not dsn:
        import os

        dsn = os.environ.get("DATABASE_URL")
    if not dsn:
        sys.exit("no database: pass -d postgresql://... or set DATABASE_URL")

    in_dir: Path = args.input
    if not in_dir.is_dir():
        sys.exit(f"input folder not found: {in_dir.resolve()}")

    try:
        import psycopg2
        from psycopg2.extras import Json
    except ImportError:
        sys.exit("needs psycopg2: pip install 'psycopg2-binary'")

    files = sorted(p for p in in_dir.glob("*.json") if not p.name.startswith("_"))
    if not files:
        sys.exit(f"no .json files in {in_dir.resolve()}")

    conn = psycopg2.connect(dsn)
    try:
        with conn:
            with conn.cursor() as cur:
                cur.execute("SELECT to_regclass('site_configs')")
                if cur.fetchone()[0] is None:
                    sys.exit(
                        "table site_configs does not exist - run "
                        "`news-scraper --db <url> --db-init` first"
                    )

                updated = 0
                inserted = 0
                unchanged = 0
                skipped = 0

                for path in files:
                    text = path.read_text(encoding="utf-8-sig")
                    try:
                        new_data = json.loads(text)
                    except json.JSONDecodeError as exc:
                        print(f"  skip {path.name}: invalid JSON ({exc})")
                        skipped += 1
                        continue
                    if not isinstance(new_data, dict):
                        print(
                            f"  skip {path.name}: top level is {type(new_data).__name__}"
                        )
                        skipped += 1
                        continue

                    host = re.sub(r"[^A-Za-z0-9._-]", "_", path.stem)

                    cur.execute(SELECT_ONE, (host,))
                    row = cur.fetchone()
                    old_data = row[0] if row else {}
                    is_new_host = row is None

                    merged, changed = merge_configs(old_data, new_data)

                    if is_new_host:
                        print(f"  {host}: new host, inserting {len(new_data)} key(s)")
                        inserted += 1
                    elif changed:
                        print(f"  {host}: updating {changed}")
                        updated += 1
                    else:
                        unchanged += 1
                        continue

                    if not args.dry_run:
                        cur.execute(UPSERT, (host, Json(merged)))

                cur.execute("SELECT count(*) FROM site_configs")
                total = cur.fetchone()[0]

            if args.dry_run:
                # roll back any accidental writes (there shouldn't be any)
                conn.rollback()

        mode = "DRY RUN - " if args.dry_run else ""
        print(
            f"{mode}{inserted} new host(s), {updated} updated, "
            f"{unchanged} unchanged, {skipped} skipped ({in_dir.resolve()})"
        )
        if not args.dry_run:
            print(f"site_configs now holds {total} row(s)")
    finally:
        conn.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
