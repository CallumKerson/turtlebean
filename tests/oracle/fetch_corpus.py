# /// script
# requires-python = ">=3.12"
# dependencies = ["beancount==3.2.0"]
# ///
"""Download the conformance corpus and record Python beancount's results.

Reads tests/corpus.toml, downloads each source at its pinned commit into
tests/corpus/<name>/, generates a bean-example ledger, then writes
FILE.beancount.json next to every ledger using tests/oracle/dump.py.

`plugin` lines and the `documents` option are commented out first, as
turtlebean doesn't run plugins. Files that crash the oracle get no expected
result and are skipped by the tests.

Usage: uv run --script tests/oracle/fetch_corpus.py
"""

import io
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "tests" / "corpus"
DUMP = ROOT / "tests" / "oracle" / "dump.py"
# Lines commented out so beancount loads the ledger without plugins
STRIPPED_PREFIXES = ("plugin ", 'option "documents"')


def download(source):
    url = f"https://codeload.github.com/{source['repo']}/tar.gz/{source['rev']}"
    request = urllib.request.Request(
        url, headers={"User-Agent": "turtlebean-corpus-fetch"}
    )
    with urllib.request.urlopen(request) as response:
        data = io.BytesIO(response.read())
    target = CORPUS / source["name"]
    prefix = source["dir"].strip("/") + "/"
    with tarfile.open(fileobj=data, mode="r:gz") as archive:
        for member in archive.getmembers():
            if not member.isfile():
                continue
            # Archives start with a "<repo>-<rev>/" directory
            inner = member.name.split("/", 1)[1] if "/" in member.name else ""
            if not inner.startswith(prefix):
                continue
            path = target / inner[len(prefix) :]
            if not path.resolve().is_relative_to(target.resolve()):
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(member).read())
    print(f"{source['name']}: {sum(1 for _ in target.rglob('*.beancount'))} ledgers")


def generate_example(settings):
    target = CORPUS / "bean-example" / "example.beancount"
    target.parent.mkdir(parents=True, exist_ok=True)
    command = Path(sys.executable).parent / "bean-example"
    subprocess.run(
        [
            str(command),
            "--seed",
            str(settings["seed"]),
            "--date-begin",
            settings["date_begin"],
            "--date-end",
            settings["date_end"],
            "--date-birth",
            settings["date_birth"],
            "--output",
            str(target),
        ],
        check=True,
    )
    print("bean-example: 1 ledger")


def strip_plugins(path):
    try:
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    except UnicodeDecodeError:
        return
    stripped = [";" + ln if ln.startswith(STRIPPED_PREFIXES) else ln for ln in lines]
    if stripped != lines:
        path.write_text("".join(stripped), encoding="utf-8")


def record(path):
    result = subprocess.run(
        [sys.executable, str(DUMP), "--write", str(path)],
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    return result.returncode == 0


def main():
    config = tomllib.loads((ROOT / "tests" / "corpus.toml").read_text(encoding="utf-8"))
    if CORPUS.exists():
        shutil.rmtree(CORPUS)
    for source in config["source"]:
        download(source)
    generate_example(config["bean_example"])

    ledgers = sorted(CORPUS.rglob("*.beancount"))
    for path in ledgers:
        strip_plugins(path)
    failed = [p for p in ledgers if not record(p)]
    print(f"recorded {len(ledgers) - len(failed)} of {len(ledgers)} ledgers")
    for path in failed:
        print(f"  oracle failed: {path.relative_to(CORPUS)}")


if __name__ == "__main__":
    main()
