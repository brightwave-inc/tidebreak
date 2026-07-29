//! Socket-level proof that the shipped page transport dials exactly the
//! address the admission policy vetted — and nothing else.
//!
//! It lives in its own test binary because it sets proxy environment
//! variables. `reqwest` reads those when a client is built, so keeping them in
//! a process of their own is what stops them from reaching another test.

#![cfg(feature = "extract-native")]

use std::net::IpAddr;
use std::time::Duration;

use openwave_web_search::{PageFetchTransport, ReqwestPageFetcher};
use tokio::net::TcpListener;
use url::Url;

/// The transport is handed one vetted address and must use it, no matter what
/// the environment would rather it did.
///
/// Two things make this fail if either defense is removed. Without
/// `resolve_to_addrs`, `pinned.invalid` is a name that cannot resolve, so no
/// socket is ever reached and the pinned listener never accepts. Without
/// `no_proxy`, the ambient proxy wins over the pinned address: the proxy
/// listener accepts and the pinned one does not.
#[tokio::test]
async fn dials_the_pinned_address_and_never_an_ambient_proxy() {
    let pinned = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pinned_address = pinned.local_addr().unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();

    std::env::set_var("HTTPS_PROXY", format!("http://{proxy_address}"));
    std::env::set_var("ALL_PROXY", format!("http://{proxy_address}"));

    let url = Url::parse(&format!(
        "https://pinned.invalid:{}/page",
        pinned_address.port()
    ))
    .unwrap();
    let fetch = tokio::spawn(async move {
        // The listener speaks no TLS, so this fetch always ends in an error.
        // Which socket it reached on the way is the whole question.
        let _ = ReqwestPageFetcher
            .get(
                &url,
                &[IpAddr::from([127, 0, 0, 1])],
                Duration::from_secs(2),
            )
            .await;
    });

    // Accepting and immediately closing ends the handshake the fetcher is
    // waiting on, so the fetch resolves without burning its whole timeout.
    let dialed = tokio::time::timeout(Duration::from_secs(5), pinned.accept())
        .await
        .is_ok();
    std::env::remove_var("HTTPS_PROXY");
    std::env::remove_var("ALL_PROXY");
    assert!(
        dialed,
        "the transport never dialed the address it was pinned to"
    );

    let _ = fetch.await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), proxy.accept())
            .await
            .is_err(),
        "the transport dialed the ambient proxy instead of the vetted address"
    );
}
