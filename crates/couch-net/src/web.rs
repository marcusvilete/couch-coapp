//! Fetching one small document over HTTPS (M26c-2, ADR 0014: the update
//! notice's look at GitHub's latest release). The only HTTP the app does
//! of its own: through the reqwest iroh already brings, on the TLS roots
//! and crypto iroh's relays use. A generic user agent and nothing of the
//! user's in the request (no referrer either); the reply is read up to a
//! limit, and redirects are followed only on the same host (a renamed
//! repository), three at most.

use std::time::Duration;

/// What the request says it is (GitHub's API refuses a request without
/// one): the app's name, nothing of the user's nor its version.
pub const USER_AGENT: &str = "couch-coapp";

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("{0}")]
    Request(String),
    #[error("the server answered {0}")]
    Status(u16),
    #[error("the answer is larger than {0} bytes")]
    TooLarge(usize),
}

/// Reads `url` with `accept` as its `Accept` header, at most `max_bytes` of
/// it, within `timeout` in all. Plain http only if `allow_http` (a
/// development or test build's stand-in server).
pub async fn get(
    url: &str,
    accept: &str,
    max_bytes: usize,
    timeout: Duration,
    allow_http: bool,
) -> Result<Vec<u8>, WebError> {
    let request = async {
        let tls = iroh::tls::CaTlsConfig::default()
            .client_config(iroh::tls::default_provider())
            .map_err(|e| WebError::Request(format!("TLS: {e}")))?;
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .user_agent(USER_AGENT)
            .https_only(!allow_http)
            .redirect(same_host_only())
            .referer(false)
            .build()
            .map_err(|e| WebError::Request(e.to_string()))?;
        let mut response = client
            .get(url)
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .await
            .map_err(|e| WebError::Request(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(WebError::Status(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(WebError::TooLarge(max_bytes));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| WebError::Request(e.without_url().to_string()))?
        {
            if body.len() + chunk.len() > max_bytes {
                return Err(WebError::TooLarge(max_bytes));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    };
    tokio::time::timeout(timeout, request)
        .await
        .map_err(|_| WebError::Request("no answer in time".into()))?
}

/// Follows a redirect only to the host first asked, three at most: nobody
/// else hears from the app.
fn same_host_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let first = attempt.previous().first().and_then(|url| url.host_str());
        if attempt.previous().len() > 3 {
            attempt.error("too many redirects")
        } else if first.is_some() && first == attempt.url().host_str() {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves one plain http answer on loopback; returns its address.
    async fn serve(answer: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(answer.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}/latest")
    }

    #[tokio::test]
    async fn reads_a_small_answer() {
        let url =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await;
        let body = get(
            &url,
            "application/json",
            1024,
            Duration::from_secs(10),
            true,
        )
        .await
        .unwrap();
        assert_eq!(body, b"{}");
    }

    #[tokio::test]
    async fn refuses_more_than_the_limit() {
        let url = serve("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n0123456789").await;
        let err = get(&url, "application/json", 4, Duration::from_secs(10), true)
            .await
            .unwrap_err();
        assert!(matches!(err, WebError::TooLarge(4)), "{err}");
    }

    #[tokio::test]
    async fn says_what_the_server_answered() {
        let url =
            serve("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let err = get(
            &url,
            "application/json",
            1024,
            Duration::from_secs(10),
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WebError::Status(404)), "{err}");
    }

    #[tokio::test]
    async fn never_follows_a_redirect_to_another_host() {
        let url = serve(
            "HTTP/1.1 302 Found
Location: http://localhost:9/elsewhere
Content-Length: 0
Connection: close

",
        )
        .await;
        let err = get(
            &url,
            "application/json",
            1024,
            Duration::from_secs(10),
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WebError::Status(302)), "{err}");
    }

    #[tokio::test]
    async fn plain_http_only_when_allowed() {
        let err = get(
            "http://127.0.0.1:9/latest",
            "application/json",
            1024,
            Duration::from_secs(10),
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WebError::Request(_)), "{err}");
    }
}
