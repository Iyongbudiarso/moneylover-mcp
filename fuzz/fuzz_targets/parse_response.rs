#![no_main]

//! Feeds arbitrary bytes, interpreted as a Money Lover API response, through the
//! parsers. They must never panic: the server parses data it does not control.

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use moneylover_mcp::error::api_error;
use moneylover_mcp::model::{Category, Transaction, Wallet, items, parse_decimal};

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let _ = api_error(&value);
    let _ = parse_decimal(&value);
    let currencies = HashMap::new();
    for item in items(&value, "data") {
        let _ = Wallet::from_value(item);
        let _ = Category::from_value(item);
        let _ = Transaction::from_value(item, chrono_tz::UTC, &currencies);
    }
    let _ = Wallet::from_value(&value);
    let _ = Category::from_value(&value);
    let _ = Transaction::from_value(&value, chrono_tz::Asia::Jakarta, &currencies);
});
