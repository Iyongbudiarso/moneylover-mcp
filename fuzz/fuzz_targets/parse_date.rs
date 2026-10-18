#![no_main]

//! Arbitrary text through the date parser, which handles both user-supplied
//! filters and timestamps from the API. It must never panic.

use libfuzzer_sys::fuzz_target;
use moneylover_mcp::model::parse_date;

fuzz_target!(|data: &str| {
    let _ = parse_date(data, chrono_tz::UTC);
    let _ = parse_date(data, chrono_tz::Asia::Jakarta);
});
