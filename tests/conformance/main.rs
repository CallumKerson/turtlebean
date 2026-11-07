//! Conformance tests: turtlebean against Python beancount.
//!
//! - `cases::*`: the hand-written cases in `tests/cases`, compared with the
//!   expected results committed next to them (`*.beancount.json`).
//! - `roundtrip::*`: every case parses and writes back byte for byte.
//! - `corpus::*`: third-party ledgers fetched by `mise run corpus:fetch` into
//!   `tests/corpus` (never committed). Files using syntax outside turtlebean's
//!   subset pass as long as turtlebean says so.
//! - `real_ledger`: runs when `TURTLEBEAN_REAL_LEDGER` names a ledger file,
//!   comparing with Python beancount live (needs `uv`).

mod render;

use std::path::{Path, PathBuf};
use std::process::Command;

use libtest_mimic::{Arguments, Failed, Trial};
use serde_json::Value;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// The plugins `turtlebean::load_file` runs; keep in step with
/// `Loader::standard`. The oracle keeps these when it strips a real
/// ledger's plugins.
const RUN_PLUGINS: &[&str] = &["beancount.plugins.auto_accounts"];

fn main() {
    let args = Arguments::from_args();
    let root = Path::new(ROOT);
    let mut trials = Vec::new();

    let cases = root.join("tests/cases");
    for path in beancount_files(&cases) {
        let name = display(&path, &cases);
        let source = path.clone();
        trials.push(Trial::test(format!("roundtrip::{name}"), move || {
            roundtrip(&source)
        }));
        let expected = json_path(&path);
        if expected.exists() {
            trials.push(Trial::test(format!("cases::{name}"), move || {
                compare_with_file(&path, &expected, false)
            }));
        }
    }

    let corpus = root.join("tests/corpus");
    for path in beancount_files(&corpus) {
        let expected = json_path(&path);
        if expected.exists() {
            let name = display(&path, &corpus);
            trials.push(Trial::test(format!("corpus::{name}"), move || {
                compare_with_file(&path, &expected, true)
            }));
        }
    }

    if let Some(ledger) = std::env::var_os("TURTLEBEAN_REAL_LEDGER") {
        let ledger = PathBuf::from(ledger);
        trials.push(Trial::test("real_ledger", move || {
            compare_with_oracle(&ledger)
        }));
    }

    libtest_mimic::run(&args, trials).exit();
}

fn beancount_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "beancount") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn json_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    path.with_file_name(name)
}

fn display(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn roundtrip(path: &Path) -> Result<(), Failed> {
    // Invalid UTF-8 can't survive a round trip through text, so compare
    // with the decoded form
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let document = turtlebean::parse(&text);
    let written = turtlebean::write(&document);
    if written == *text {
        Ok(())
    } else {
        Err(diff(&text, &written).into())
    }
}

fn compare_with_file(path: &Path, expected: &Path, allow_unsupported: bool) -> Result<(), Failed> {
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(expected).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    compare(path, &expected, allow_unsupported)
}

fn compare_with_oracle(path: &Path) -> Result<(), Failed> {
    let output = Command::new("uv")
        .args(["run", "--quiet", "--script"])
        .arg(Path::new(ROOT).join("tests/oracle/dump.py"))
        .arg("--strip-plugins")
        .args(RUN_PLUGINS.iter().flat_map(|name| ["--keep-plugin", name]))
        .arg(path)
        .output()
        .map_err(|e| format!("running the oracle: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let expected: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let mut ledger = turtlebean::load_file(path);
    // The oracle had these plugins stripped, so it has nothing to match them.
    ledger.errors.retain(|e| !is_unrun_plugin(e));
    compare_ledger(path, &ledger, &expected)
}

fn is_unrun_plugin(error: &turtlebean::Error) -> bool {
    error.kind == turtlebean::ErrorKind::Unsupported
        && error.source.is_none()
        && error.message.starts_with("turtlebean doesn't run plugin")
}

fn compare(path: &Path, expected: &Value, allow_unsupported: bool) -> Result<(), Failed> {
    let ledger = turtlebean::load_file(path);
    if allow_unsupported
        && ledger
            .errors
            .iter()
            .any(|e| e.kind == turtlebean::ErrorKind::Unsupported)
    {
        return Ok(());
    }
    compare_ledger(path, &ledger, expected)
}

fn compare_ledger(
    path: &Path,
    ledger: &turtlebean::Ledger,
    expected: &Value,
) -> Result<(), Failed> {
    let root = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let actual = render::render(ledger, &root);
    if &actual == expected {
        return Ok(());
    }
    // Set TURTLEBEAN_DUMP_DIR to keep both sides for closer inspection
    if let Some(dir) = std::env::var_os("TURTLEBEAN_DUMP_DIR") {
        let dir = PathBuf::from(dir);
        let write = |name: &str, value: &Value| {
            let text = serde_json::to_string_pretty(value).unwrap_or_default();
            std::fs::write(dir.join(name), text).map_err(|e| e.to_string())
        };
        write("expected.json", expected)?;
        write("actual.json", &actual)?;
    }
    let summary = summarise(expected, &actual);
    let expected = serde_json::to_string_pretty(expected).unwrap_or_default();
    let actual = serde_json::to_string_pretty(&actual).unwrap_or_default();
    Err(format!("{summary}\n{}", diff(&expected, &actual)).into())
}

/// A one-line hint at what differs, before the full diff.
fn summarise(expected: &Value, actual: &Value) -> String {
    let parts: Vec<&str> = ["errors", "entries", "balances"]
        .into_iter()
        .filter(|k| expected[k] != actual[k])
        .collect();
    format!("differs in: {}", parts.join(", "))
}

fn diff(expected: &str, actual: &str) -> String {
    similar::TextDiff::from_lines(expected, actual)
        .unified_diff()
        .context_radius(3)
        .header("expected", "actual")
        .to_string()
}
