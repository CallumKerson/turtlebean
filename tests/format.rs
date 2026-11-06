//! `format::entry` writes text that loads back to the same directives, and
//! `format::align` changes nothing else, checked on every conformance case and
//! corpus ledger that loads without errors.

use std::path::{Path, PathBuf};

use turtlebean::{Entry, Origin, format};

fn ledgers() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut found = Vec::new();
    let mut pending = vec![root.join("cases"), root.join("corpus")];
    while let Some(dir) = pending.pop() {
        let Ok(items) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in items.flatten() {
            let path = item.path();
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

/// What should survive formatting: everything but where it was written.
fn essence(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.origin == Origin::Written)
        .map(|e| format!("{} {:?} {:?}", e.date, e.kind, e.meta))
        .collect()
}

#[test]
fn formatted_entries_load_back_the_same() {
    let scratch = std::env::temp_dir().join(format!("turtlebean-format-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut checked = 0;
    for path in ledgers() {
        let ledger = turtlebean::load_file(&path);
        // Includes would need their files beside the copy.
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if !ledger.errors.is_empty() || text.contains("include ") {
            continue;
        }
        // Options and plugins aren't directives, so carry them over as written.
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with("option ") || line.starts_with("plugin ") {
                out.push_str(line);
                out.push('\n');
            }
        }
        for entry in ledger
            .entries
            .iter()
            .filter(|e| e.origin == Origin::Written)
        {
            out.push('\n');
            out.push_str(&format::entry(entry));
        }
        let out = format::align(&out);
        let copy = scratch.join("main.beancount");
        std::fs::write(&copy, &out).unwrap();
        let reloaded = turtlebean::load_file(&copy);
        assert_eq!(reloaded.errors, [], "{}:\n{out}", path.display());
        assert_eq!(
            essence(&reloaded.entries),
            essence(&ledger.entries),
            "{}:\n{out}",
            path.display()
        );
        checked += 1;
    }
    std::fs::remove_dir_all(&scratch).unwrap();
    assert!(checked > 20, "only {checked} ledgers checked");
}

#[test]
fn align_only_changes_spacing() {
    for path in ledgers() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let aligned = format::align(&text);
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(squash(&aligned), squash(&text), "{}", path.display());
        assert_eq!(
            format::align(&aligned),
            aligned,
            "{} isn't stable",
            path.display()
        );
    }
}
