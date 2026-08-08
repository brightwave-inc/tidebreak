//! Server-owned connectors — loopback OAuth (RFC 8252 + PKCE) for model-gateway
//! and ChatGPT subscription sign-in, plus the future source connector tools
//! that ingest from Drive, Box, and friends.
//!
//! [`GatewayAuth`] speaks to a model-gateway deployment's own OAuth 2.0
//! authorization server. The gateway's endpoints are fixed relative to its
//! base URL, so the client pins the deployment through `/api/v1/meta`'s
//! `installation_id` and refuses tokens minted by any other installation.
//!
//! [`ChatGptAuth`] speaks the Codex-compatible ChatGPT subscription OAuth
//! surface (`auth.openai.com`, fixed localhost:1455 redirect) and stores the
//! resulting tokens for the OpenAI provider's subscription auth mode.
//!
//! The flow is authorization code + PKCE S256 on a loopback redirect: the
//! desktop opens the returned authorization URL in the system browser, the
//! browser lands on a short-lived local listener, and the code is exchanged
//! for tokens. Gateway sessions mint narrower audiences (`llm`,
//! `mcp:<endpoint>`) on demand through resource-scoped refresh; ChatGPT
//! sessions refresh a single access token.
//!
//! Tokens are stored through the host's [`SecretProvider`] — the OS keychain
//! in the desktop app — never in ordinary settings, and none of the types in
//! this module include token material in their `Debug` output.
//!
//! [`SecretProvider`]: openwave_core::SecretProvider

mod chatgpt;
mod gateway;

pub use chatgpt::{
    has_stored_chatgpt_credentials, is_chatgpt_sign_in_required, ChatGptAuth, ChatGptAuthConfig,
    ChatGptAuthorizedSession, ChatGptConnection, ChatGptCredentialVault, ChatGptCredentials,
    ChatGptPendingSignIn, CALLBACK_PORT, CLIENT_ID, CODEX_BASE_URL, ORIGINATOR, REDIRECT_URI,
    SECRET_KEY as CHATGPT_SECRET_KEY,
};
pub use gateway::{
    has_stored_credentials, has_stored_credentials_for, is_sign_in_required,
    validate_mcp_endpoint_slug, AuthorizedSession, CredentialVault, GatewayApp, GatewayAuth,
    GatewayAuthConfig, GatewayConnection, GatewayCredentials, GatewayIdentity, GatewayMeta,
    GatewayModel, PendingSignIn, TokenSet, RESOURCE_CONTROL, RESOURCE_LLM,
};
