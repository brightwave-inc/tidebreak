//! Single-use tokens behind the sandboxed view frames.
//!
//! An iframe cannot carry the API bearer, so frame documents are reached by
//! capability instead: the authenticated renderer trades its bearer for a
//! short-lived token at a mint route, and the unauthenticated frame route
//! redeems the token exactly once. The table is its own small piece of app
//! state rather than part of the MCP runtime because tokens address stored
//! local-app revisions as well as prefetched MCP views, and the MCP runtime
//! has no other reason to be involved in app frames.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tidebreak_core::id::{AppId, AppRevisionId};
use tokio::sync::Mutex;

/// How long a minted view-frame token stays redeemable. One iframe load
/// consumes it; a remount mints a fresh one.
const VIEW_FRAME_TOKEN_TTL: Duration = Duration::from_secs(60);
/// Bound on outstanding tokens, so a mint loop cannot grow the table.
const MAX_VIEW_FRAME_TOKENS: usize = 64;

/// What one frame token addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewFrameSource {
    /// A prefetched MCP Apps view, re-resolved against the live runtime at
    /// redemption so a server that dropped its view stops serving it.
    McpView {
        /// Configured server namespace the view was prefetched from.
        server: String,
        /// Declared view URI under that server.
        uri: String,
    },
    /// A stored local-app revision, addressed by durable identity only.
    /// Redemption loads the revision's write-once bundle bytes.
    AppRevision {
        /// App the revision belongs to.
        app_id: AppId,
        /// Exact revision to serve.
        revision_id: AppRevisionId,
    },
}

/// Outstanding single-use view-frame tokens.
#[derive(Default)]
pub(crate) struct ViewFrameTokens {
    tokens: Mutex<HashMap<uuid::Uuid, (ViewFrameSource, Instant)>>,
}

impl ViewFrameTokens {
    /// Mint a token addressing `source`, sweeping expired tokens first.
    /// Returns `None` at the outstanding-token bound.
    pub(crate) async fn mint(&self, source: ViewFrameSource) -> Option<uuid::Uuid> {
        let token = uuid::Uuid::new_v4();
        let mut tokens = self.tokens.lock().await;
        let now = Instant::now();
        tokens.retain(|_, (_, minted)| now.duration_since(*minted) < VIEW_FRAME_TOKEN_TTL);
        if tokens.len() >= MAX_VIEW_FRAME_TOKENS {
            return None;
        }
        tokens.insert(token, (source, now));
        Some(token)
    }

    /// Redeem a token, consuming it. An expired token is consumed too: the
    /// capability is spent by presentation, whether or not it was served.
    pub(crate) async fn take(&self, token: uuid::Uuid) -> Option<ViewFrameSource> {
        let mut tokens = self.tokens.lock().await;
        let (source, minted) = tokens.remove(&token)?;
        if minted.elapsed() >= VIEW_FRAME_TOKEN_TTL {
            return None;
        }
        Some(source)
    }

    #[cfg(test)]
    async fn backdate(&self, token: uuid::Uuid, age: Duration) {
        let mut tokens = self.tokens.lock().await;
        if let Some((_, minted)) = tokens.get_mut(&token) {
            *minted = Instant::now() - age;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of the capability contract the HTTP tests cannot reach:
    /// expiry refuses service but still consumes the token, and the
    /// outstanding set is bounded with expired tokens swept at mint.
    #[tokio::test]
    async fn tokens_are_single_use_bounded_and_expire() {
        let tokens = ViewFrameTokens::default();
        let source = ViewFrameSource::McpView {
            server: "gateway".into(),
            uri: "ui://fixture/app.html".into(),
        };

        let token = tokens.mint(source.clone()).await.unwrap();
        assert_eq!(tokens.take(token).await, Some(source.clone()));
        assert_eq!(
            tokens.take(token).await,
            None,
            "redemption spends the token"
        );

        let expired = tokens.mint(source.clone()).await.unwrap();
        tokens.backdate(expired, VIEW_FRAME_TOKEN_TTL).await;
        assert_eq!(
            tokens.take(expired).await,
            None,
            "an expired token is never served, and presenting it spends it"
        );

        let mut outstanding = Vec::new();
        for _ in 0..MAX_VIEW_FRAME_TOKENS {
            outstanding.push(tokens.mint(source.clone()).await.unwrap());
        }
        assert!(
            tokens.mint(source.clone()).await.is_none(),
            "the outstanding set is bounded"
        );
        for token in outstanding {
            tokens.backdate(token, VIEW_FRAME_TOKEN_TTL).await;
        }
        assert!(
            tokens.mint(source).await.is_some(),
            "expired tokens are swept at mint rather than counted forever"
        );
    }
}
