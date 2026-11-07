# /// script
# requires-python = ">=3.12"
# dependencies = ["beancount==3.2.0"]
# ///
"""Load ledgers with Python beancount and print what it makes of them as JSON.

This is the conformance oracle: turtlebean's tests render their own results in
the same shape and compare. It uses only beancount's public loader and data
types, and runs beancount as a separate program.

Usage:
    uv run --script tests/oracle/dump.py FILE [FILE ...]
    uv run --script tests/oracle/dump.py --write FILE [FILE ...]
    uv run --script tests/oracle/dump.py --strip-plugins [--keep-plugin NAME ...] FILE

--write saves each result next to its input as FILE.json instead of printing.
--strip-plugins loads a copy of the ledger's directory with `plugin` lines and
the `documents` option removed, for comparing real ledgers whose plugins
turtlebean doesn't run. --keep-plugin leaves a plugin that turtlebean does run.
"""

import argparse
import datetime
import json
import shutil
import sys
import tempfile
from decimal import Decimal
from pathlib import Path

from beancount import loader
from beancount.core import amount, data, position


def number(value):
    if value is None:
        return None
    # "f" never uses exponents, matching how turtlebean prints decimals
    return format(value, "f")


def render_amount(value):
    if value is None:
        return None
    return {"number": number(value.number), "currency": value.currency}


def render_cost(value):
    if value is None:
        return None
    if isinstance(value, position.CostSpec):
        # Only appears when booking failed and left the spec unresolved
        return {"unresolved": True}
    return {
        "number": number(value.number),
        "currency": value.currency,
        "date": value.date.isoformat() if value.date else None,
        "label": value.label,
    }


def render_value(value):
    if isinstance(value, bool):
        return {"bool": value}
    if isinstance(value, Decimal):
        return {"number": number(value)}
    if isinstance(value, datetime.date):
        return {"date": value.isoformat()}
    if isinstance(value, amount.Amount):
        return {"amount": render_amount(value)}
    if isinstance(value, str):
        return {"string": value}
    if value is None:
        return {"null": None}
    return {"other": repr(value)}


def render_meta(meta):
    if not meta:
        return {}
    return {
        key: render_value(value)
        for key, value in sorted(meta.items())
        if key not in ("filename", "lineno") and not key.startswith("__")
    }


def relative(filename, root):
    if not filename or filename.startswith("<"):
        return filename
    try:
        return Path(filename).resolve().relative_to(root).as_posix()
    except ValueError:
        return filename


def render_entry(entry, root):
    meta = entry.meta or {}
    out = {
        "type": type(entry).__name__,
        "date": entry.date.isoformat(),
        "file": relative(meta.get("filename"), root),
        "line": meta.get("lineno"),
        "meta": render_meta(meta),
    }
    if isinstance(entry, data.Open):
        out["account"] = entry.account
        out["currencies"] = entry.currencies
        out["booking"] = entry.booking.name if entry.booking else None
    elif isinstance(entry, data.Close):
        out["account"] = entry.account
    elif isinstance(entry, data.Commodity):
        out["currency"] = entry.currency
    elif isinstance(entry, data.Balance):
        out["account"] = entry.account
        out["amount"] = render_amount(entry.amount)
        out["tolerance"] = number(entry.tolerance)
    elif isinstance(entry, data.Pad):
        out["account"] = entry.account
        out["source_account"] = entry.source_account
    elif isinstance(entry, data.Price):
        out["currency"] = entry.currency
        out["amount"] = render_amount(entry.amount)
    elif isinstance(entry, data.Custom):
        out["custom_type"] = entry.type
        out["values"] = [
            {
                "dtype": getattr(v.dtype, "__name__", str(v.dtype)),
                **render_value(v.value),
            }
            for v in entry.values
        ]
    elif isinstance(entry, data.Transaction):
        out["flag"] = entry.flag
        out["payee"] = entry.payee
        out["narration"] = entry.narration
        out["tags"] = sorted(entry.tags or ())
        out["links"] = sorted(entry.links or ())
        out["postings"] = [
            {
                "account": p.account,
                "units": render_amount(p.units)
                if isinstance(p.units, amount.Amount) and p.units.number is not None
                else None,
                "cost": render_cost(p.cost),
                "price": render_amount(p.price),
                "flag": p.flag,
                "meta": render_meta(p.meta),
            }
            for p in entry.postings
        ]
    else:
        out["unsupported"] = True
    return out


def render_error(error, root):
    source = error.source or {}
    return {
        "kind": type(error).__name__,
        "file": relative(source.get("filename"), root),
        "line": source.get("lineno"),
    }


def balances(entries):
    totals = {}
    for entry in entries:
        if not isinstance(entry, data.Transaction):
            continue
        for p in entry.postings:
            if not isinstance(p.units, amount.Amount) or p.units.number is None:
                continue
            account = totals.setdefault(p.account, {})
            account[p.units.currency] = (
                account.get(p.units.currency, Decimal(0)) + p.units.number
            )
    return {
        account: {
            currency: number(n) for currency, n in sorted(currencies.items()) if n != 0
        }
        for account, currencies in sorted(totals.items())
        if any(n != 0 for n in currencies.values())
    }


def dump(path, root):
    entries, errors, _options = loader.load_file(str(path))
    rendered_errors = [render_error(e, root) for e in errors]
    rendered_errors.sort(key=lambda e: (e["file"] or "", e["line"] or 0, e["kind"]))
    return {
        "errors": rendered_errors,
        "entries": [render_entry(e, root) for e in entries],
        "balances": balances(entries),
    }


def strip_plugins(path, keep):
    """Copy the ledger's directory, dropping the documents option and every
    plugin not in `keep`."""
    source_root = path.resolve().parent
    target = Path(tempfile.mkdtemp(prefix="turtlebean-oracle-")).resolve()
    shutil.copytree(
        source_root, target, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".git")
    )
    for file in target.rglob("*.beancount"):
        lines = file.read_text(encoding="utf-8").splitlines(keepends=True)
        kept = [";" + ln if stripped(ln, keep) else ln for ln in lines]
        file.write_text("".join(kept), encoding="utf-8")
    return target, target / path.name


def stripped(line, keep):
    if line.startswith('option "documents"'):
        return True
    if not line.startswith("plugin "):
        return False
    name = line.split('"')[1] if line.count('"') >= 2 else ""
    return name not in keep


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("files", nargs="+", type=Path)
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--strip-plugins", action="store_true")
    parser.add_argument("--keep-plugin", action="append", default=[])
    args = parser.parse_args()

    for path in args.files:
        if args.strip_plugins:
            root, target = strip_plugins(path, set(args.keep_plugin))
            try:
                result = dump(target, root)
            finally:
                shutil.rmtree(root)
        else:
            result = dump(path, path.resolve().parent)
        text = json.dumps(result, indent=1, ensure_ascii=False) + "\n"
        if args.write:
            path.with_name(path.name + ".json").write_text(text, encoding="utf-8")
        else:
            sys.stdout.write(text)


if __name__ == "__main__":
    main()
