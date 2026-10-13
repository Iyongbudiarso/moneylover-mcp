//! Turning Money Lover HTTP responses into JSON or a typed error.

use reqwest::{RequestBuilder, Response, StatusCode};
use serde_json::Value;

use crate::error::{Error, Result, api_error};

const MAX_BODY_IN_ERROR: usize = 120;
/// Far above any real response (a large account's category list is well under 1 MiB).
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Sends a request and returns the JSON body. HTTP-level failures become
/// `Transient` (429, 5xx, network) or `Protocol`. A JSON body that carries an
/// API error is returned as-is, because Money Lover reports most errors with
/// HTTP 200 and callers decide what each code means.
pub async fn send_json(request: RequestBuilder) -> Result<Value> {
    let response = request.send().await?;
    let status = response.status();
    if is_transient(status) {
        return Err(Error::Transient(format!("HTTP {status}")));
    }
    let text = read_capped(response).await?;
    match serde_json::from_str::<Value>(&text) {
        Ok(body) if status.is_success() || api_error(&body).is_some() => Ok(body),
        Ok(_) => Err(Error::Protocol(format!("HTTP {status}"))),
        Err(_) if !status.is_success() => Err(Error::Protocol(format!("HTTP {status}"))),
        Err(_) => Err(Error::Protocol(format!(
            "response is not JSON: {}",
            first_chars(&text, MAX_BODY_IN_ERROR)
        ))),
    }
}

async fn read_capped(mut response: Response) -> Result<String> {
    let too_large = || Error::Protocol(format!("response larger than {MAX_RESPONSE_BYTES} bytes"));
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn is_transient(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn first_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
