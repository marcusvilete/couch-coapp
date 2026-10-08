//! A relay server in the test process: iroh's own `iroh-relay`, the server
//! we deploy (M7), over plain HTTP on loopback. With a token it admits only
//! clients presenting it, as `access.shared_token` does in production.

use std::{
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use iroh_relay::server::{Access, AccessControl, ClientRequest, RelayConfig, Server, ServerConfig};

/// A running relay; it stops when dropped.
pub struct TestRelay {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    refused: Arc<AtomicUsize>,
    _server: Server,
}

impl TestRelay {
    /// Starts one. `token`: the only token it admits, or `None` for anyone.
    pub async fn start(token: Option<&str>) -> Self {
        let refused = Arc::new(AtomicUsize::new(0));
        let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.key_cache_capacity = Some(1024);
        relay.access = Arc::new(TokenAccess {
            token: token.map(str::to_string),
            refused: refused.clone(),
        });
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        let server = Server::spawn(config).await.expect("the test relay starts");
        let addr = server.http_addr().expect("an http relay");
        Self {
            url: format!("http://{addr}"),
            refused,
            _server: server,
        }
    }

    /// How many connections it turned away for a missing or wrong token.
    pub fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct TokenAccess {
    token: Option<String>,
    refused: Arc<AtomicUsize>,
}

impl AccessControl for TokenAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        match &self.token {
            Some(token) if request.auth_token().as_ref() != Some(token) => {
                self.refused.fetch_add(1, Ordering::SeqCst);
                Access::Deny {
                    reason: Some("this relay is for Couch Co-App".into()),
                }
            }
            _ => Access::Allow,
        }
    }
}
