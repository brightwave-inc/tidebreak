//! The global default citation format, and how a chat resolves against it.
//!
//! A chat carries its own choice only when the user made one; otherwise it
//! follows whatever the install is set to, so changing the default moves every
//! conversation that never overrode it. The stored value is the format's token,
//! under the same store-settings convention as the model roles.

use openwave_core::{CitationFormat, Result, Store};

/// Store setting key holding the install-wide default.
const SETTING_KEY: &str = "citation_format";

/// The install-wide default, or `None` when the user has not set one.
pub async fn read_default(store: &dyn Store) -> Result<Option<CitationFormat>> {
    Ok(store
        .get_setting(SETTING_KEY)
        .await?
        .and_then(|value| value.as_str().and_then(CitationFormat::from_str)))
}

/// Persist the install-wide default. `None` clears it back to the product
/// default, stored as JSON null so [`read_default`] reads it back as unset.
pub async fn write_default(store: &dyn Store, format: Option<CitationFormat>) -> Result<()> {
    let value = format.map_or(serde_json::Value::Null, |format| {
        serde_json::json!(format.as_str())
    });
    store.set_setting(SETTING_KEY, &value).await
}

/// The format a turn in this chat runs under: the chat's own choice, else the
/// install default, else the product default.
pub async fn resolve(store: &dyn Store, chat: Option<CitationFormat>) -> Result<CitationFormat> {
    match chat {
        Some(format) => Ok(format),
        None => Ok(read_default(store).await?.unwrap_or_default()),
    }
}
