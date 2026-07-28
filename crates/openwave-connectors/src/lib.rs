//! OpenWave connectors — loopback OAuth (RFC 8252 + PKCE) for model-gateway,
//! plus the future source connector tools that ingest from Drive, Box, and
//! friends.
//!
//! [`GatewayAuth`] speaks to a model-gateway deployment's own OAuth 2.0
//! authorization server. The gateway's endpoints are fixed relative to its
//! base URL, so the client pins the deployment through `/api/v1/meta`'s
//! `installation_id` and refuses tokens minted by any other installation.
//!
//! The flow is authorization code + PKCE S256 on a loopback redirect: the
//! desktop opens the returned authorization URL in the system browser, the
//! browser lands on a short-lived local listener, and the code is exchanged
//! for a `control`-resource token. Narrower audiences (`llm`,
//! `mcp:<endpoint>`) are minted on demand through resource-scoped refresh.
//!
//! Tokens are stored through the host's [`SecretProvider`] — the OS keychain
//! in the desktop app — never in ordinary settings, and none of the types in
//! this crate include token material in their `Debug` output.
//!
//! [`SecretProvider`]: openwave_core::SecretProvider

mod gateway;

pub use gateway::{
    has_stored_credentials, is_sign_in_required, validate_mcp_endpoint_slug, AuthorizedSession,
    CredentialVault, GatewayApp, GatewayAuth, GatewayAuthConfig, GatewayConnection,
    GatewayCredentials, GatewayIdentity, GatewayMeta, GatewayModel, PendingSignIn, TokenSet,
    RESOURCE_CONTROL, RESOURCE_LLM,
};
