//! Renders turtlebean's results in the same JSON shape as `tests/oracle/dump.py`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use turtlebean::Decimal;
use turtlebean::{
    Amount, Cost, CustomType, Entry, EntryKind, Error, ErrorKind, Ledger, MetaValue, Posting,
    Source,
};

pub fn render(ledger: &Ledger, root: &Path) -> Value {
    let mut errors: Vec<Value> = ledger
        .errors
        .iter()
        .map(|e| render_error(e, root))
        .collect();
    errors.sort_by_key(|e| {
        (
            e["file"].as_str().unwrap_or_default().to_owned(),
            e["line"].as_u64().unwrap_or_default(),
            e["kind"].as_str().unwrap_or_default().to_owned(),
        )
    });
    json!({
        "errors": errors,
        "entries": ledger.entries.iter().map(|e| render_entry(e, root)).collect::<Vec<_>>(),
        "balances": balances(&ledger.entries),
    })
}

fn relative(source: &Source, root: &Path) -> String {
    source
        .file
        .strip_prefix(root)
        .unwrap_or(&source.file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn render_error(error: &Error, root: &Path) -> Value {
    let (file, line) = match (&error.kind, &error.source) {
        (ErrorKind::LoadError, _) | (_, None) => ("<load>".to_owned(), 0),
        (_, Some(source)) => (relative(source, root), source.line),
    };
    json!({ "kind": error.kind.name(), "file": file, "line": line })
}

fn render_amount(amount: Option<&Amount>) -> Value {
    amount.map_or(
        Value::Null,
        |a| json!({ "number": a.number.to_string(), "currency": a.currency }),
    )
}

fn render_cost(cost: Option<&Cost>) -> Value {
    cost.map_or(Value::Null, |c| {
        json!({
            "number": c.number.to_string(),
            "currency": c.currency,
            "date": c.date.to_string(),
            "label": c.label,
        })
    })
}

fn render_value(value: &MetaValue) -> Value {
    match value {
        MetaValue::String(s) => json!({ "string": s }),
        MetaValue::Number(n) => json!({ "number": n.to_string() }),
        MetaValue::Date(d) => json!({ "date": d.to_string() }),
        MetaValue::Bool(b) => json!({ "bool": b }),
        MetaValue::Amount(a) => json!({ "amount": render_amount(Some(a)) }),
        MetaValue::Null => json!({ "null": null }),
    }
}

fn render_meta(meta: &[(String, MetaValue)]) -> Value {
    let sorted: BTreeMap<&str, Value> = meta
        .iter()
        .map(|(k, v)| (k.as_str(), render_value(v)))
        .collect();
    json!(sorted)
}

fn render_posting(posting: &Posting) -> Value {
    json!({
        "account": posting.account,
        "units": render_amount(posting.units.as_ref()),
        "cost": render_cost(posting.cost.as_ref()),
        "price": render_amount(posting.price.as_ref()),
        "flag": posting.flag.map(String::from),
        "meta": render_meta(&posting.meta),
    })
}

fn render_entry(entry: &Entry, root: &Path) -> Value {
    let mut out = json!({
        "type": entry.kind.type_name(),
        "date": entry.date.to_string(),
        "file": relative(&entry.source, root),
        "line": entry.source.line,
        "meta": render_meta(&entry.meta),
    });
    let fields = match &entry.kind {
        EntryKind::Open {
            account,
            currencies,
            booking,
        } => json!({
            "account": account,
            "currencies": currencies,
            "booking": booking.map(turtlebean::Booking::name),
        }),
        EntryKind::Close { account } => json!({ "account": account }),
        EntryKind::Commodity { currency } => json!({ "currency": currency }),
        EntryKind::Balance {
            account,
            amount,
            tolerance,
        } => json!({
            "account": account,
            "amount": render_amount(Some(amount)),
            "tolerance": tolerance.map(|t| t.to_string()),
        }),
        EntryKind::Pad {
            account,
            source_account,
        } => json!({
            "account": account,
            "source_account": source_account,
        }),
        EntryKind::Price { currency, amount } => json!({
            "currency": currency,
            "amount": render_amount(Some(amount)),
        }),
        EntryKind::Custom { type_name, values } => json!({
            "custom_type": type_name,
            "values": values.iter().map(|v| {
                let value = render_value(&v.value);
                let dtype = match v.dtype {
                    CustomType::String => "str",
                    CustomType::Account => "<AccountDummy>",
                    CustomType::Date => "date",
                    CustomType::Amount => "Amount",
                    CustomType::Bool => "bool",
                    CustomType::Number => "Decimal",
                };
                let mut out = serde_json::Map::new();
                out.insert("dtype".into(), json!(dtype));
                if let Value::Object(fields) = value {
                    out.extend(fields);
                }
                Value::Object(out)
            }).collect::<Vec<_>>(),
        }),
        EntryKind::Transaction(txn) => json!({
            "flag": txn.flag.to_string(),
            "payee": txn.payee,
            "narration": txn.narration,
            "tags": txn.tags,
            "links": txn.links,
            "postings": txn.postings.iter().map(render_posting).collect::<Vec<_>>(),
        }),
    };
    if let (Value::Object(out), Value::Object(fields)) = (&mut out, fields) {
        out.extend(fields);
    }
    out
}

fn balances(entries: &[Entry]) -> Value {
    let mut totals: BTreeMap<&str, BTreeMap<&str, Decimal>> = BTreeMap::new();
    for entry in entries {
        let EntryKind::Transaction(txn) = &entry.kind else {
            continue;
        };
        for posting in &txn.postings {
            let Some(units) = &posting.units else {
                continue;
            };
            let total = totals
                .entry(&posting.account)
                .or_default()
                .entry(&units.currency)
                .or_default();
            *total = python_round(*total + units.number);
        }
    }
    let mut out = serde_json::Map::new();
    for (account, currencies) in totals {
        let non_zero: serde_json::Map<String, Value> = currencies
            .into_iter()
            .filter(|(_, n)| !n.is_zero())
            .map(|(c, n)| (c.to_owned(), json!(n.to_string())))
            .collect();
        if !non_zero.is_empty() {
            out.insert(account.to_owned(), Value::Object(non_zero));
        }
    }
    Value::Object(out)
}

/// Rounds to 28 significant digits, as Python's sums in the oracle do.
fn python_round(value: Decimal) -> Decimal {
    let mantissa = value.mantissa().unsigned_abs();
    let digits = if mantissa == 0 {
        1
    } else {
        mantissa.ilog10() + 1
    };
    match digits
        .checked_sub(28)
        .and_then(|excess| value.scale().checked_sub(excess))
    {
        Some(places) if digits > 28 => {
            value.round_dp_with_strategy(places, turtlebean::RoundingStrategy::MidpointNearestEven)
        }
        _ => value,
    }
}
