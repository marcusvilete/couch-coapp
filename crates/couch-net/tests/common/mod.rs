//! Shared setup for the integration tests: two real endpoints on loopback.

use couch_net::{
    Connection, Endpoint, EndpointAddr, control,
    endpoint::{self, EndpointOptions},
};

pub async fn bind(options: EndpointOptions) -> Endpoint {
    // Timers (netem's included) are otherwise as coarse as 15.6 ms.
    #[cfg(windows)]
    couch_platform_win::timer::request_1ms_resolution();
    endpoint::bind(options).await.expect("bind")
}

/// Connects `viewer` to `host` at `addr` and runs the control handshake.
/// Returns (host side, viewer side).
pub async fn connect(
    host: &Endpoint,
    viewer: &Endpoint,
    addr: EndpointAddr,
) -> (Connection, Connection) {
    let dial = async {
        let conn = viewer
            .connect(addr, couch_proto::ALPN)
            .await
            .expect("connect");
        control::open(&conn).await.expect("viewer handshake");
        conn
    };
    let accept = async {
        let incoming = host.accept().await.expect("incoming");
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        control::accept(&conn).await.expect("host handshake");
        conn
    };
    let (viewer_conn, host_conn) = tokio::join!(dial, accept);
    (host_conn, viewer_conn)
}
