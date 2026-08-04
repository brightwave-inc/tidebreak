//! Voice transcription settings and the credential-custody cloud adapter.

use axum::body::Bytes;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use openwave_core::{SecretProvider, Store};

use crate::error::ServerError;
use crate::providers::{self, ProviderCredential, ProviderKind};
use crate::state::{LocalVoiceError, LocalVoiceRunner, LocalVoiceState};

const SETTING_KEY: &str = "voice.transcription_v1";
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTranscriptionModel {
    #[default]
    Local,
    Gpt4oTranscribe,
    GeminiFlash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct VoiceTranscriptionConfig {
    model: VoiceTranscriptionModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct VoiceTranscriptionInfo {
    pub model: VoiceTranscriptionModel,
    pub local: LocalVoiceInfo,
    pub openai_ready: bool,
    pub gemini_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct LocalVoiceInfo {
    pub state: String,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
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
    local_voice: &dyn LocalVoiceRunner,
) -> Result<VoiceTranscriptionInfo, ServerError> {
    let config = read_config(store).await?;
    let openai = providers::read_config(store, ProviderKind::Openai).await?;
    let gemini = providers::read_config(store, ProviderKind::Gemini).await?;
    let local = local_voice.status().await;
    Ok(VoiceTranscriptionInfo {
        model: config.model,
        local: LocalVoiceInfo {
            state: match local.state {
                LocalVoiceState::NotInstalled => "not_installed",
                LocalVoiceState::Downloading => "downloading",
                LocalVoiceState::Ready => "ready",
                LocalVoiceState::Failed => "failed",
                LocalVoiceState::Unavailable => "unavailable",
            }
            .to_owned(),
            downloaded_bytes: local.downloaded_bytes,
            total_bytes: local.total_bytes,
            error: local.error,
        },
        openai_ready: openai.enabled
            && providers::has_credential(secrets, ProviderKind::Openai).await,
        gemini_ready: gemini.enabled
            && providers::has_credential(secrets, ProviderKind::Gemini).await,
    })
}

pub async fn update(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    local_voice: &dyn LocalVoiceRunner,
    update: VoiceTranscriptionUpdate,
) -> Result<VoiceTranscriptionInfo, ServerError> {
    let current = info(store, secrets, local_voice).await?;
    match update.model {
        VoiceTranscriptionModel::Gpt4oTranscribe if !current.openai_ready => {
            return Err(ServerError::conflict(
                "enable OpenAI and save its credential before selecting gpt-4o-transcribe",
            ))
        }
        VoiceTranscriptionModel::GeminiFlash if !current.gemini_ready => {
            return Err(ServerError::conflict(
                "enable Gemini and save a supported credential before selecting Gemini voice input",
            ))
        }
        _ => {}
    }
    let value = serde_json::to_value(VoiceTranscriptionConfig {
        model: update.model,
    })
    .map_err(|_| ServerError::internal("failed to serialize voice transcription settings"))?;
    store.set_setting(SETTING_KEY, &value).await?;
    info(store, secrets, local_voice).await
}

pub async fn install_local(
    local_voice: &dyn LocalVoiceRunner,
) -> Result<LocalVoiceInfo, ServerError> {
    let status = local_voice.install().await.map_err(ServerError::internal)?;
    Ok(LocalVoiceInfo {
        state: match status.state {
            LocalVoiceState::NotInstalled => "not_installed",
            LocalVoiceState::Downloading => "downloading",
            LocalVoiceState::Ready => "ready",
            LocalVoiceState::Failed => "failed",
            LocalVoiceState::Unavailable => "unavailable",
        }
        .to_owned(),
        downloaded_bytes: status.downloaded_bytes,
        total_bytes: status.total_bytes,
        error: status.error,
    })
}

pub async fn transcribe(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    local_voice: &dyn LocalVoiceRunner,
    content_type: &str,
    audio: Bytes,
) -> Result<String, ServerError> {
    if audio.is_empty() {
        return Err(ServerError::bad_request("voice recording is empty"));
    }
    let config = read_config(store).await?;
    if config.model == VoiceTranscriptionModel::Local {
        return local_voice
            .transcribe(content_type, audio.to_vec())
            .await
            .map_err(|error| match error {
                LocalVoiceError::UnsupportedMedia(message) => {
                    ServerError::unsupported_media_type_kind("voice_recording_unsupported", message)
                }
                LocalVoiceError::Undecodable(message) => {
                    ServerError::unprocessable_kind("voice_recording_undecodable", message)
                }
                LocalVoiceError::Runner(message) => ServerError::internal(message),
            });
    }
    if config.model == VoiceTranscriptionModel::GeminiFlash {
        return transcribe_gemini(store, secrets, content_type, &audio).await;
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

async fn transcribe_gemini(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    content_type: &str,
    audio: &[u8],
) -> Result<String, ServerError> {
    const MODEL: &str = "gemini-3.6-flash";
    let config = providers::read_config(store, ProviderKind::Gemini).await?;
    if !config.enabled {
        return Err(ServerError::conflict("Gemini is not enabled"));
    }
    let credential = providers::read_credential(secrets, ProviderKind::Gemini).await?;
    let provider = match credential {
        Some(ProviderCredential::ApiKey { key }) if !key.is_empty() => {
            openwave_router::GeminiProvider::new(key)
        }
        Some(credential) => {
            let json = credential
                .as_service_account()
                .ok_or_else(|| ServerError::conflict("Gemini has no supported saved credential"))?;
            let account = openwave_router::GoogleServiceAccount::from_json(json)
                .map_err(|_| ServerError::conflict("Gemini service account is invalid"))?;
            let project = account.project_id().to_owned();
            let source = std::sync::Arc::new(
                openwave_router::GoogleServiceAccountTokenSource::new(account),
            );
            openwave_router::GeminiProvider::vertex(
                project,
                config.vertex_location.as_deref().unwrap_or("global"),
                source,
            )
            .map_err(ServerError::from)?
        }
        None => {
            let key = providers::resolve_api_key(secrets, ProviderKind::Gemini)
                .await
                .ok_or_else(|| ServerError::conflict("Gemini has no saved credential"))?;
            openwave_router::GeminiProvider::new(key)
        }
    };
    provider
        .transcribe_audio(MODEL, content_type, &BASE64.encode(audio))
        .await
        .map_err(ServerError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LocalVoiceState, LocalVoiceStatus};
    use openwave_core::{DbStore, Result as CoreResult};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestSecrets(Mutex<HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> CoreResult<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> CoreResult<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> CoreResult<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    async fn test_store() -> (tempfile::TempDir, DbStore) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("voice.sqlite");
        let store = DbStore::connect(&format!("sqlite://{}?mode=rwc", database.display()))
            .await
            .unwrap();
        (directory, store)
    }

    struct ReadyLocal;

    #[async_trait::async_trait]
    impl LocalVoiceRunner for ReadyLocal {
        async fn status(&self) -> LocalVoiceStatus {
            LocalVoiceStatus {
                state: LocalVoiceState::Ready,
                downloaded_bytes: Some(10),
                total_bytes: Some(10),
                error: None,
            }
        }

        async fn install(&self) -> Result<LocalVoiceStatus, String> {
            Ok(self.status().await)
        }

        async fn transcribe(
            &self,
            content_type: &str,
            audio: Vec<u8>,
        ) -> Result<String, LocalVoiceError> {
            assert_eq!(content_type, "audio/webm");
            assert_eq!(audio, b"local audio");
            Ok("local transcript".into())
        }
    }

    #[tokio::test]
    async fn local_selection_delegates_bytes_to_the_native_runner() {
        let (_directory, store) = test_store().await;
        let secrets = TestSecrets::default();
        let text = transcribe(
            &store,
            &secrets,
            &ReadyLocal,
            "audio/webm",
            Bytes::from_static(b"local audio"),
        )
        .await
        .unwrap();
        assert_eq!(text, "local transcript");
    }

    #[tokio::test]
    async fn info_projects_local_lifecycle_without_paths_or_model_bytes() {
        let (_directory, store) = test_store().await;
        let info = info(&store, &TestSecrets::default(), &ReadyLocal)
            .await
            .unwrap();
        assert_eq!(info.local.state, "ready");
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("model.bin"));
        assert!(!json.contains("models/voice"));
    }
}
