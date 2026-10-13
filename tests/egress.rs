//! Locks down where this server may connect to. The only network destinations are
//! Money Lover's own API hosts; adding any other URL to the source fails this test,
//! so a change in egress cannot slip in unnoticed during review.

use std::{fs, path::Path};

use moneylover_mcp::config::{Config, DEFAULT_API_BASE, DEFAULT_OAUTH_BASE};

const ALLOWED_HOSTS: [&str; 2] = ["web.moneylover.me", "oauth.moneylover.me"];

#[test]
fn default_endpoints_are_money_lover_over_https() {
    for base in [DEFAULT_API_BASE, DEFAULT_OAUTH_BASE] {
        let url = reqwest::Url::parse(base).unwrap();
        assert_eq!(url.scheme(), "https", "{base}");
        assert!(ALLOWED_HOSTS.contains(&url.host_str().unwrap()), "{base}");
    }
}

#[test]
fn endpoints_cannot_be_redirected_through_the_environment() {
    let cfg = Config::from_lookup(|key| match key {
        "HOME" => Some("/h".into()),
        // Plausible names someone might try; none of them may have any effect.
        "MONEYLOVER_API_BASE"
        | "MONEYLOVER_OAUTH_BASE"
        | "MONEYLOVER_API_URL"
        | "MONEYLOVER_BASE_URL" => Some("https://attacker.example".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.api_base, DEFAULT_API_BASE);
    assert_eq!(cfg.oauth_base, DEFAULT_OAUTH_BASE);
}

#[test]
fn source_contains_no_other_network_destinations() {
    let mut offenders = Vec::new();
    for file in rust_files(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path()) {
        let text = fs::read_to_string(&file).unwrap();
        for url in urls_in(&text) {
            let allowed = reqwest::Url::parse(&url).is_ok_and(|u| {
                u.scheme() == "https" && u.host_str().is_some_and(|h| ALLOWED_HOSTS.contains(&h))
            });
            if !allowed {
                offenders.push(format!("{}: {url}", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "unexpected network destinations in src/ (update ALLOWED_HOSTS only after review):\n{}",
        offenders.join("\n")
    );
}

fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    files
}

/// Every `http://`, `https://`, `ws://` or `wss://` literal in `text`.
fn urls_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        let mut rest = text;
        while let Some(start) = rest.find(scheme) {
            let tail = &rest[start..];
            let end = tail
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | '>' | ',' | ';')
                })
                .unwrap_or(tail.len());
            // A bare scheme (e.g. `starts_with("https://")`) names no destination.
            if end > scheme.len() {
                found.push(tail[..end].to_string());
            }
            rest = &tail[scheme.len()..];
        }
    }
    found
}

#[test]
fn url_scanner_finds_urls() {
    let found = urls_in(
        r#"let a = "https://evil.example/x"; // see http://b.example, wss://c.example; s.starts_with("https://")"#,
    );
    assert_eq!(
        found,
        [
            "http://b.example",
            "https://evil.example/x",
            "wss://c.example"
        ]
    );
}
