#!/usr/bin/env python3
"""
Merge JSON selector files: for each file in `new_selectors_merged`, override
the matching file in `selectors_merged` with any non-empty fields, writing
the result to `merged_output` (filenames are matched by name, e.g. civic.am.json).

Empty means: "", [], {}, or None. Any such value in the "new" file is
skipped (the original value is kept). Nested dicts (e.g. "_meta") are merged
recursively using the same rule.

Usage:
    python merge_selectors.py \
        --base selectors_merged \
        --override new_selectors_merged \
        --out merged_output

If --out is omitted, files are merged in place (the base folder is overwritten).
"""

import argparse
import json
import sys
from pathlib import Path


def is_empty(value):
    """Treat "", [], {}, None as 'no data'."""
    if value is None:
        return True
    if isinstance(value, (str, list, dict)) and len(value) == 0:
        return True
    return False


def merge_dicts(base: dict, override: dict) -> dict:
    """Recursively override `base` with non-empty values from `override`."""
    result = dict(base)  # shallow copy is fine; we only recurse into dicts
    for key, new_value in override.items():
        if key not in result:
            # Field doesn't exist in base at all -> only add if it has data
            if not is_empty(new_value):
                result[key] = new_value
            continue

        old_value = result[key]

        if isinstance(new_value, dict) and isinstance(old_value, dict):
            result[key] = merge_dicts(old_value, new_value)
        elif not is_empty(new_value):
            result[key] = new_value
        # else: new_value is empty -> keep old_value as-is

    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default="selectors_merged", help="Folder with original JSON files")
    parser.add_argument("--override", default="new_selectors_merged", help="Folder with override JSON files")
    parser.add_argument("--out", default=None, help="Output folder (default: overwrite --base in place)")
    args = parser.parse_args()

    base_dir = Path(args.base)
    override_dir = Path(args.override)
    out_dir = Path(args.out) if args.out else base_dir

    if not base_dir.is_dir():
        sys.exit(f"Base folder not found: {base_dir}")
    if not override_dir.is_dir():
        sys.exit(f"Override folder not found: {override_dir}")

    out_dir.mkdir(parents=True, exist_ok=True)

    base_files = {p.name: p for p in base_dir.glob("*.json")}
    override_files = {p.name: p for p in override_dir.glob("*.json")}

    merged_count = 0
    skipped_no_base = []
    skipped_no_override = []

    for name, override_path in override_files.items():
        base_path = base_files.get(name)
        if base_path is None:
            skipped_no_base.append(name)
            continue

        with open(base_path, "r", encoding="utf-8") as f:
            base_data = json.load(f)
        with open(override_path, "r", encoding="utf-8") as f:
            override_data = json.load(f)

        merged = merge_dicts(base_data, override_data)

        out_path = out_dir / name
        with open(out_path, "w", encoding="utf-8") as f:
            json.dump(merged, f, ensure_ascii=False, indent=2)
            f.write("\n")

        merged_count += 1

    # Files that exist only in base (no override) -> copy through unchanged
    # so the output folder is a complete set, if writing to a separate folder.
    if out_dir != base_dir:
        for name, base_path in base_files.items():
            if name not in override_files:
                with open(base_path, "r", encoding="utf-8") as f:
                    data = json.load(f)
                out_path = out_dir / name
                with open(out_path, "w", encoding="utf-8") as f:
                    json.dump(data, f, ensure_ascii=False, indent=2)
                    f.write("\n")
                skipped_no_override.append(name)

    print(f"Merged {merged_count} file(s) into: {out_dir}")
    if skipped_no_base:
        print(f"Skipped (no matching base file): {len(skipped_no_base)} -> {skipped_no_base}")
    if skipped_no_override:
        print(f"Copied unchanged (no override present): {len(skipped_no_override)}")


if __name__ == "__main__":
    main()
