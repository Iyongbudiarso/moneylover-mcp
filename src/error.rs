use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("Money Lover API error {code}: {message}")]
    Api { code: i64, message: String },
    /// Network failure, timeout, HTTP 429 or HTTP 5xx. The only retryable kind.
    #[error("network problem: {0}")]
    Transient(String),
    #[error("unexpected response from Money Lover: {0}")]
    Protocol(String),
    /// Bad input from the caller (or an unresolvable wallet/category name).
    #[error("{0}")]
    Invalid(String),
    #[error("token storage error: {0}")]
    Storage(String),
    /// A write was sent but no usable answer came back: it may or may not have been saved.
    #[error("{0}")]
    OutcomeUnknown(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Error::Transient(_))
    }
}

/// Tool handlers report failures to the MCP client as plain text.
impl From<Error> for String {
    fn from(e: Error) -> Self {
        e.to_string()
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        let e = e.without_url();
        // reqwest's own message is just "error sending request"; the real cause is in the source chain.
        Error::Transient(format!(
            "{}: {e}{}",
            network_failure_kind(&e),
            cause_chain(&e)
        ))
    }
}

fn network_failure_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_request() {
        "request failed"
    } else if e.is_body() || e.is_decode() {
        "response body failed"
    } else {
        "network error"
    }
}

fn cause_chain(e: &dyn std::error::Error) -> String {
    std::iter::successors(e.source(), |cause| cause.source())
        .map(|cause| format!(" -> {cause}"))
        .collect()
}

/// Money Lover answers HTTP 200 even for failures, so errors live in the body.
/// Two shapes exist: `{error: <n>, msg}` and `{s: false, e: <n>, message}`.
pub fn api_error(body: &Value) -> Option<(i64, String)> {
    let code = ["error", "e"]
        .iter()
        .filter_map(|key| body.get(*key).and_then(Value::as_i64))
        .find(|code| *code != 0)?;
    let message = ["msg", "message"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_str))
        .unwrap_or("unknown error");
    Some((code, message.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_both_error_shapes() {
        assert_eq!(
            api_error(&json!({"error": 3, "msg": "bad"})),
            Some((3, "bad".into()))
        );
        assert_eq!(
            api_error(&json!({"s": false, "e": 706, "message": "token"})),
            Some((706, "token".into()))
        );
    }

    #[test]
    fn success_is_not_an_error() {
        assert_eq!(api_error(&json!({"error": 0, "data": []})), None);
        assert_eq!(api_error(&json!({"data": []})), None);
    }
}
