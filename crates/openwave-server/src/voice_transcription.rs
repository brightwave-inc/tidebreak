//! Voice transcription settings and the credential-custody cloud adapter.

use axum::body::Bytes;
use serde::{Deserialize, Serialize};

use openwave_core::{SecretProvider, Store};

use crate::error::ServerError;
use crate::providers::{self, ProviderKind};

const SETTING_KEY: &str = "voice.transcription_v1";
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTranscriptionModel {
    Local,
    Gpt4oTranscribe,
}

impl Default for VoiceTranscriptionModel {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct VoiceTranscriptionConfig {
    model: VoiceTranscriptionModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct VoiceTranscriptionInfo {
    pub model: VoiceTranscriptionModel,
    pub local_ready: bool,
    pub openai_ready: bool,
}

#[derive(Debug, Deserialize)]
pub struct VoiceTranscriptionUpdate {
    pub model: VoiceTranscriptionModel,
}

async fn read_config(store: &dyn Store) -> openwave_core::Result<VoiceTranscriptionConfig> {
    Ok(store
        .get_setting(SETTING_KEY)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

pub async fn info(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<VoiceTranscriptionInfo, ServerError> {
    let config = read_config(store).await?;
    let openai = providers::read_config(store, ProviderKind::Openai).await?;
    Ok(VoiceTranscriptionInfo {
        model: config.model,
        // Deliberately false until the pinned lazy local runner lands.
        local_ready: false,
        openai_ready: openai.enabled
            && providers::has_credential(secrets, ProviderKind::Openai).await,
    })
}

pub async fn update(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    update: VoiceTranscriptionUpdate,
) -> Result<VoiceTranscriptionInfo, ServerError> {
    if update.model == VoiceTranscriptionModel::Gpt4oTranscribe {
        let current = info(store, secrets).await?;
        if !current.openai_ready {
            return Err(ServerError::conflict(
                "enable OpenAI and save its credential before selecting gpt-4o-transcribe",
            ));
        }
    }
    let value = serde_json::to_value(VoiceTranscriptionConfig {
        model: update.model,
    })
    .map_err(|_| ServerError::internal("failed to serialize voice transcription settings"))?;
    store.set_setting(SETTING_KEY, &value).await?;
    info(store, secrets).await
}

pub async fn transcribe(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    content_type: &str,
    audio: Bytes,
) -> Result<String, ServerError> {
    if audio.is_empty() {
        return Err(ServerError::bad_request("voice recording is empty"));
    }
    let config = read_config(store).await?;
    if config.model == VoiceTranscriptionModel::Local {
        return Err(ServerError::conflict(
            "the recommended local transcription model is not installed in this build yet",
        ));
    }
    let provider = providers::read_config(store, ProviderKind::Openai).await?;
    if !provider.enabled {
        return Err(ServerError::conflict("OpenAI is not enabled"));
    }
    let key = providers::read_credential(secrets, ProviderKind::Openai)
        .await?
        .and_then(|credential| credential.as_api_key().map(ToOwned::to_owned))
        .ok_or_else(|| ServerError::conflict("OpenAI has no saved credential"))?;
    let extension = match content_type.split(';').next().unwrap_or("") {
        "audio/mp4" => "m4a",
        "audio/webm" => "webm",
        _ => {
            return Err(ServerError::bad_request(
                "unsupported voice recording format",
            ))
        }
    };
    let part = reqwest::multipart::Part::bytes(audio.to_vec())
        .file_name(format!("recording.{extension}"))
        .mime_str(content_type)
        .map_err(|_| ServerError::bad_request("invalid voice recording content type"))?;
    let response = reqwest::Client::new()
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(key)
        .multipart(
            reqwest::multipart::Form::new()
                .text("model", "gpt-4o-transcribe")
                .part("file", part),
        )
        .send()
        .await
        .map_err(|_| ServerError::internal("OpenAI transcription request failed"))?;
    if !response.status().is_success() {
        return Err(ServerError::internal(format!(
            "OpenAI transcription failed with status {}",
            response.status()
        )));
    }
    #[derive(Deserialize)]
    struct Transcript {
        text: String,
    }
    let transcript: Transcript = response
        .json()
        .await
        .map_err(|_| ServerError::internal("OpenAI returned an invalid transcription response"))?;
    Ok(transcript.text)
}
