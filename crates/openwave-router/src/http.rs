//! Shared HTTP client construction and stream deadlines for provider adapters.
//!
//! A provider connection that hangs is indistinguishable, from the agent loop's
//! point of view, from a model that is thinking: the loop only wakes on cancel
//! or steer, and the turn worker keeps heartbeating its lease. Without a
//! timeout somewhere in the transport, a stalled socket stalls the turn until
//! the user presses Stop.
//!
//! Two layers guard that, and they guard different failures:
//!
//! * A **dead-air read timeout** ([`ProviderTimeouts::dead_air`]) — reqwest's
//!   `read_timeout` resets on every successful read, so it fires only when *no*
//!   bytes at all arrive for the whole window. Token-by-token generation, however
//!   slow, keeps resetting it; a silently dropped connection does not. This is
//!   the distinction that matters, and it is why the client-level total
//!   [`timeout`](reqwest::ClientBuilder::timeout) is deliberately not set on the
//!   streaming client — that one is a wall-clock deadline on the whole response
//!   and would cut healthy long generations off mid-turn.
//! * A **total stream ceiling** ([`ProviderTimeouts::total_stream`]) — a stream
//!   that trickles a byte often enough to keep resetting the read timeout would
//!   otherwise run forever. [`with_stream_deadline`] caps the whole thing.
//!
//! Both are configurable, because a local model on consumer hardware and a
//! hosted frontier model have very different latency profiles:
//!
//! | Variable | Default |
//! |---|---|
//! | `OPENWAVE_PROVIDER_CONNECT_TIMEOUT_SECS` | 10 |
//! | `OPENWAVE_PROVIDER_READ_TIMEOUT_SECS` | 300 |
//! | `OPENWAVE_PROVIDER_STREAM_TIMEOUT_SECS` | 3600 |
//!
//! Setting any of them to `0` disables that timeout. An unparseable value is
//! ignored in favor of the default rather than failing provider construction.

use std::sync::LazyLock;
use std::time::Duration;

use futures::{Stream, StreamExt};

use openwave_core::error::Result;

/// Provider-specific authentication applied to an already-shaped HTTP request.
///
/// Most adapters own a fixed header scheme, but Bedrock Mantle exposes existing
/// Messages and Responses wire contracts behind either a bearer key or AWS
/// Signature Version 4. Keeping authentication at this boundary lets those
/// adapters retain their mature request/stream normalization while signing the
/// exact bytes they send.
pub(crate) trait RequestAuthenticator: Send + Sync {
    fn authenticate(
        &self,
        request: reqwest::RequestBuilder,
        url: &reqwest::Url,
        body: &[u8],
    ) -> Result<reqwest::RequestBuilder>;
}

/// Connect-phase budget. Loopback is instant and a reachable hosted provider
/// completes TCP + TLS well inside this; anything slower is unreachable, not
/// slow.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Dead-air budget: the longest a healthy stream may deliver *zero* bytes.
///
/// Deliberately more generous than a server-side default would be. A local
/// model ingesting a long prompt on CPU can spend minutes before the first
/// token, with nothing on the wire in the meantime, and a spurious mid-turn
/// failure is worse for that user than the hang this guards against. Hosted
/// providers emit SSE keep-alives far inside the window.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Wall-clock ceiling on a single provider stream. One model step running for
/// an hour is pathological under any latency profile.
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(3600);

/// Total budget for a non-streaming provider call (token exchange, and other
/// small request/response round trips). These have a known, small body, so a
/// plain total deadline is the right shape.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The transport deadlines applied to every provider adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderTimeouts {
    /// Connect-phase budget (DNS + TCP + TLS).
    pub connect: Option<Duration>,
    /// Longest gap between reads before the connection is declared dead.
    pub dead_air: Option<Duration>,
    /// Ceiling on the total duration of one provider stream.
    pub total_stream: Option<Duration>,
}

impl ProviderTimeouts {
    /// Read the deadlines from the environment, falling back to the defaults.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            connect: env_duration(
                "OPENWAVE_PROVIDER_CONNECT_TIMEOUT_SECS",
                DEFAULT_CONNECT_TIMEOUT,
            ),
            dead_air: env_duration("OPENWAVE_PROVIDER_READ_TIMEOUT_SECS", DEFAULT_READ_TIMEOUT),
            total_stream: env_duration(
                "OPENWAVE_PROVIDER_STREAM_TIMEOUT_SECS",
                DEFAULT_STREAM_TIMEOUT,
            ),
        }
    }
}

/// `None` means "disabled": an explicit `0`, or a default of zero.
fn env_duration(name: &str, default: Duration) -> Option<Duration> {
    let seconds = std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map_or(default, Duration::from_secs);
    (!seconds.is_zero()).then_some(seconds)
}

/// The process-wide deadlines. Read once; provider construction must not depend
/// on when in the process lifetime it happens.
pub fn timeouts() -> &'static ProviderTimeouts {
    static TIMEOUTS: LazyLock<ProviderTimeouts> = LazyLock::new(ProviderTimeouts::from_env);
    &TIMEOUTS
}

fn build(builder: reqwest::ClientBuilder) -> reqwest::Client {
    let builder = match timeouts().connect {
        Some(connect) => builder.connect_timeout(connect),
        None => builder,
    };
    // Matches `reqwest::Client::new`, which panics on the same failure: the
    // only way this fails is a TLS backend that could not initialize, and a
    // provider adapter has nothing useful to do without one.
    builder
        .build()
        .expect("provider HTTP client: TLS backend failed to initialize")
}

/// A client for streaming provider requests: connect and dead-air deadlines,
/// and deliberately no total-response deadline (see the module docs).
///
/// Pair it with [`with_stream_deadline`] on the response body to get the total
/// ceiling.
#[must_use]
pub fn streaming_client() -> reqwest::Client {
    let builder = reqwest::Client::builder();
    let builder = match timeouts().dead_air {
        Some(dead_air) => builder.read_timeout(dead_air),
        None => builder,
    };
    build(builder)
}

/// A client for small, non-streaming provider calls, under a total deadline.
#[must_use]
pub fn request_client() -> reqwest::Client {
    build(reqwest::Client::builder().timeout(DEFAULT_REQUEST_TIMEOUT))
}

/// Why a provider byte stream stopped early.
///
/// Adapters already treat a mid-stream error as a hard failure (the step's
/// tool-call arguments may be truncated mid-JSON), so the ceiling reuses that
/// path rather than inventing a second one.
#[derive(Debug)]
pub enum StreamFailure<E> {
    /// The underlying transport failed — including reqwest's own dead-air read
    /// timeout, which surfaces here as an ordinary body error.
    Transport(E),
    /// The stream outlived [`ProviderTimeouts::total_stream`].
    Deadline(Duration),
}

impl<E: std::fmt::Display> std::fmt::Display for StreamFailure<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Deadline(limit) => {
                write!(f, "exceeded the {}s stream duration limit", limit.as_secs())
            }
        }
    }
}

impl<E: std::fmt::Display> StreamFailure<E> {
    /// The client-safe form of the failure: only the deadline text is ours.
    ///
    /// A transport error's `Display` is reqwest's, and reqwest includes the
    /// request URL — a Vertex URL carries the project id, and any gateway URL
    /// may carry tenant-identifying parts. These strings reach the client
    /// through `ProviderEvent::Failed` and `TurnFailed`, so a transport
    /// failure reports only the fact of an early end.
    #[must_use]
    pub fn client_message(&self, provider: &str) -> String {
        match self {
            Self::Deadline(_) => format!("{provider} stream ended early: {self}"),
            Self::Transport(_) => format!("{provider} stream ended early"),
        }
    }
}

/// Cap the total duration of a provider byte stream.
///
/// The returned stream forwards items unchanged until `ceiling` elapses, then
/// yields a single [`StreamFailure::Deadline`] and ends. A `None` ceiling
/// forwards the stream untouched.
pub fn with_stream_deadline<S, B, E>(
    stream: S,
    ceiling: Option<Duration>,
) -> impl Stream<Item = Result<B, StreamFailure<E>>>
where
    S: Stream<Item = Result<B, E>>,
{
    async_stream::stream! {
        futures::pin_mut!(stream);
        let deadline = ceiling.map(|ceiling| tokio::time::Instant::now() + ceiling);
        loop {
            let next = match (deadline, ceiling) {
                (Some(deadline), Some(ceiling)) => {
                    match tokio::time::timeout_at(deadline, stream.next()).await {
                        Ok(next) => next,
                        Err(_) => {
                            yield Err(StreamFailure::Deadline(ceiling));
                            return;
                        }
                    }
                }
                _ => stream.next().await,
            };
            match next {
                Some(Ok(item)) => yield Ok(item),
                Some(Err(error)) => {
                    yield Err(StreamFailure::Transport(error));
                    return;
                }
                None => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceiling must cut a stream that is still *making progress* — that is
    /// the case the dead-air read timeout can never catch, since every trickled
    /// byte resets it.
    #[tokio::test(start_paused = true)]
    async fn ceiling_ends_a_stream_that_trickles_forever() {
        let trickle = async_stream::stream! {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                yield Ok::<_, String>(b"data: ping\n\n".to_vec());
            }
        };
        let capped = with_stream_deadline(trickle, Some(Duration::from_secs(120)));
        futures::pin_mut!(capped);

        let mut delivered = 0;
        let ending = loop {
            match capped.next().await {
                Some(Ok(_)) => delivered += 1,
                other => break other,
            }
        };

        assert_eq!(
            delivered, 4,
            "chunks arriving inside the ceiling pass through"
        );
        let message = match ending {
            Some(Err(failure)) => failure.to_string(),
            other => panic!("expected a deadline failure, got {other:?}"),
        };
        assert_eq!(message, "exceeded the 120s stream duration limit");
    }

    #[test]
    fn the_client_message_keeps_the_transport_error_and_its_url_out() {
        // reqwest's error display includes the request URL; a Vertex URL
        // carries the project id, and these strings reach the client.
        let transport = StreamFailure::Transport(
            "error sending request for url (https://us-central1-aiplatform.googleapis.com/v1/projects/secret-project/): connection closed".to_string(),
        );
        let message = transport.client_message("gemini");
        assert_eq!(message, "gemini stream ended early");
        assert!(!message.contains("http"), "{message}");
        assert!(!message.contains("secret-project"), "{message}");

        // The deadline text is our own and says why the stream was cut.
        let deadline = StreamFailure::<String>::Deadline(Duration::from_secs(3600));
        assert_eq!(
            deadline.client_message("anthropic"),
            "anthropic stream ended early: exceeded the 3600s stream duration limit"
        );
    }
}
