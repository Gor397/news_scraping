#!/usr/bin/env python3
"""
Upload merged selector JSON files into Postgres.

Each <name>.json becomes one row in `site_configs`:

    host   TEXT  = file stem ("example.com.json" -> "example.com")
    config JSONB = the whole file, verbatim

Requires the schema from `news-scraper --db-init` (see db.rs SCHEMA).

Examples:
    python upload_selectors.py -d postgresql://user:pass@localhost:5432/news
    python upload_selectors.py -i selectors_merged -d $DATABASE_URL
    python upload_selectors.py -i new_selectors_merged -d $DATABASE_URL --replace
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

UPSERT = """
INSERT INTO site_configs (host, config, updated_at)
VALUES (%s, %s, now())
ON CONFLICT (host) DO UPDATE
   SET config = EXCLUDED.config,
       updated_at = now()
"""

REPLACE = """
INSERT INTO site_configs (host, config, updated_at)
VALUES (%s, %s, now())
ON CONFLICT (host) DO UPDATE
   SET config = EXCLUDED.config,
       updated_at = now()
"""

DELETE_ALL = "DELETE FROM site_configs"


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("-i", "--input", default="selectors_merged", type=Path,
                    help="folder with merged selector files (default: selectors_merged)")
    ap.add_argument("-d", "--database", default=None,
                    help="postgres URL, e.g. postgresql://user:pass@localhost:5432/news "
                         "(default: DATABASE_URL env var)")
    ap.add_argument("--replace", action="store_true",
                    help="delete all existing rows before uploading")
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

    files = sorted(
        p for p in in_dir.glob("*.json")
        if not p.name.startswith("_")
    )
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
                if args.replace:
                    cur.execute(DELETE_ALL)
                    print(f"deleted all existing site_configs rows")

                uploaded = 0
                for path in files:
                    text = path.read_text(encoding="utf-8-sig")
                    try:
                        data = json.loads(text)
                    except json.JSONDecodeError as exc:
                        print(f"  skip {path.name}: invalid JSON ({exc})")
                        continue
                    if not isinstance(data, dict):
                        print(f"  skip {path.name}: top level is {type(data).__name__}")
                        continue

                    host = re.sub(r"[^A-Za-z0-9._-]", "_", path.stem)
                    cur.execute(UPSERT, (host, Json(data)))
                    uploaded += 1

                cur.execute("SELECT count(*) FROM site_configs")
                total = cur.fetchone()[0]
        print(f"uploaded {uploaded} file(s) from {in_dir.resolve()}")
        print(f"site_configs now holds {total} row(s)")
    finally:
        conn.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
