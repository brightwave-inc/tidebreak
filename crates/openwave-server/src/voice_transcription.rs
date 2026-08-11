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

/// The whisper.cpp model repository revision every local artifact is pinned to.
///
/// One revision for the whole catalog: the artifacts are content-addressed by
/// the SHA-256 below, so the revision only decides which files exist.
pub const LOCAL_VOICE_REPO_COMMIT: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

/// One installable local speech model.
///
/// The catalog is a plain list because that is all it needs to be: adding a
/// model is one entry with its artifact name, exact size, and exact digest.
/// `file`/`sha256` never reach the renderer — the desktop runner uses them to
/// fetch and verify, and the wire projection carries only what the picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalVoiceModel {
    pub id: &'static str,
    pub label: &'static str,
    /// One line on the quality/speed trade-off, shown under the label.
    pub description: &'static str,
    pub bytes: u64,
    /// English-only models transcribe faster and refuse other languages.
    pub english_only: bool,
    pub recommended: bool,
    pub file: &'static str,
    pub sha256: &'static str,
}

pub const DEFAULT_LOCAL_VOICE_MODEL: &str = "tiny.en-q5_1";

pub const LOCAL_VOICE_MODELS: &[LocalVoiceModel] = &[
    LocalVoiceModel {
        id: "tiny.en-q5_1",
        label: "Whisper tiny (English)",
        description: "Fastest and smallest. Accurate enough for short dictation.",
        bytes: 32_166_155,
        english_only: true,
        recommended: true,
        file: "ggml-tiny.en-q5_1.bin",
        sha256: "c77c5766f1cef09b6b7d47f21b546cbddd4157886b3b5d6d4f709e91e66c7c2b",
    },
    LocalVoiceModel {
        id: "base.en-q5_1",
        label: "Whisper base (English)",
        description: "Clearly more accurate than tiny on names and long sentences, still fast.",
        bytes: 59_721_011,
        english_only: true,
        recommended: false,
        file: "ggml-base.en-q5_1.bin",
        sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
    },
    LocalVoiceModel {
        id: "small.en-q5_1",
        label: "Whisper small (English)",
        description: "Best English accuracy that still runs comfortably on a laptop CPU.",
        bytes: 190_098_681,
        english_only: true,
        recommended: false,
        file: "ggml-small.en-q5_1.bin",
        sha256: "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30",
    },
    LocalVoiceModel {
        id: "small-q5_1",
        label: "Whisper small (multilingual)",
        description: "Same size as small English, and transcribes other languages.",
        bytes: 190_085_487,
        english_only: false,
        recommended: false,
        file: "ggml-small-q5_1.bin",
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    },
    LocalVoiceModel {
        id: "large-v3-turbo-q5_0",
        label: "Whisper large v3 turbo (multilingual)",
        description: "Highest accuracy. Wants a fast machine and over half a gigabyte of disk.",
        bytes: 574_041_195,
        english_only: false,
        recommended: false,
        file: "ggml-large-v3-turbo-q5_0.bin",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
];

pub fn local_voice_model(id: &str) -> Option<&'static LocalVoiceModel> {
    LOCAL_VOICE_MODELS.iter().find(|model| model.id == id)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTranscriptionModel {
    #[default]
    Local,
    Gpt4oTranscribe,
    GeminiFlash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VoiceTranscriptionConfig {
    model: VoiceTranscriptionModel,
    /// Which catalog entry the local option means. Kept beside the provider
    /// choice rather than folded into it so switching to the cloud and back
    /// does not forget the model already downloaded.
    #[serde(default = "default_local_model")]
    local_model: String,
}

fn default_local_model() -> String {
    DEFAULT_LOCAL_VOICE_MODEL.to_owned()
}

impl Default for VoiceTranscriptionConfig {
    fn default() -> Self {
        Self {
            model: VoiceTranscriptionModel::default(),
            local_model: default_local_model(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct VoiceTranscriptionInfo {
    pub model: VoiceTranscriptionModel,
    /// The selected catalog entry's id, whether or not local is the active
    /// provider.
    pub local_model: String,
    pub local_models: Vec<LocalVoiceModelInfo>,
    pub openai_ready: bool,
    pub gemini_ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct LocalVoiceModelInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub total_bytes: u64,
    pub english_only: bool,
    pub recommended: bool,
    pub state: String,
    pub downloaded_bytes: Option<u64>,
    pub error: Option<String>,
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
    #[serde(default)]
    pub local_model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocalVoiceInstall {
    /// Which model to install. Absent means the one currently selected.
    #[serde(default)]
    pub model: Option<String>,
}

async fn read_config(store: &dyn Store) -> openwave_core::Result<VoiceTranscriptionConfig> {
    let mut config: VoiceTranscriptionConfig = store
        .get_setting(SETTING_KEY)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    // A model can leave the catalog; a stale selection must not strand voice
    // input on an artifact nothing can install.
    if local_voice_model(&config.local_model).is_none() {
        config.local_model = default_local_model();
    }
    Ok(config)
}

fn state_name(state: LocalVoiceState) -> &'static str {
    match state {
        LocalVoiceState::NotInstalled => "not_installed",
        LocalVoiceState::Downloading => "downloading",
        LocalVoiceState::Ready => "ready",
        LocalVoiceState::Failed => "failed",
        LocalVoiceState::Unavailable => "unavailable",
    }
}

pub async fn info(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    local_voice: &dyn LocalVoiceRunner,
) -> Result<VoiceTranscriptionInfo, ServerError> {
    let config = read_config(store).await?;
    let openai = providers::read_config(store, ProviderKind::Openai).await?;
    let gemini = providers::read_config(store, ProviderKind::Gemini).await?;
    let mut local_models = Vec::with_capacity(LOCAL_VOICE_MODELS.len());
    for model in LOCAL_VOICE_MODELS {
        let status = local_voice.status(model.id).await;
        local_models.push(LocalVoiceModelInfo {
            id: model.id.to_owned(),
            label: model.label.to_owned(),
            description: model.description.to_owned(),
            total_bytes: model.bytes,
            english_only: model.english_only,
            recommended: model.recommended,
            state: state_name(status.state).to_owned(),
            downloaded_bytes: status.downloaded_bytes,
            error: status.error,
        });
    }
    Ok(VoiceTranscriptionInfo {
        model: config.model,
        local_model: config.local_model,
        local_models,
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
    let local_model = match update.local_model {
        Some(id) if local_voice_model(&id).is_none() => {
            return Err(ServerError::bad_request("unknown local voice model"))
        }
        Some(id) => id,
        None => current.local_model,
    };
    let value = serde_json::to_value(VoiceTranscriptionConfig {
        model: update.model,
        local_model,
    })
    .map_err(|_| ServerError::internal("failed to serialize voice transcription settings"))?;
    store.set_setting(SETTING_KEY, &value).await?;
    info(store, secrets, local_voice).await
}

pub async fn install_local(
    store: &dyn Store,
    local_voice: &dyn LocalVoiceRunner,
    request: LocalVoiceInstall,
) -> Result<LocalVoiceInfo, ServerError> {
    let id = match request.model {
        Some(id) => {
            if local_voice_model(&id).is_none() {
                return Err(ServerError::bad_request("unknown local voice model"));
            }
            id
        }
        None => read_config(store).await?.local_model,
    };
    let status = local_voice
        .install(&id)
        .await
        .map_err(ServerError::internal)?;
    Ok(LocalVoiceInfo {
        state: state_name(status.state).to_owned(),
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
            .transcribe(&config.local_model, content_type, audio.to_vec())
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
        Some(_) => {
            return Err(ServerError::conflict(
                "Gemini has no supported saved credential",
            ));
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
        async fn status(&self, _model: &str) -> LocalVoiceStatus {
            LocalVoiceStatus {
                state: LocalVoiceState::Ready,
                downloaded_bytes: Some(10),
                total_bytes: Some(10),
                error: None,
            }
        }

        async fn install(&self, model: &str) -> Result<LocalVoiceStatus, String> {
            Ok(self.status(model).await)
        }

        async fn transcribe(
            &self,
            model: &str,
            content_type: &str,
            audio: Vec<u8>,
        ) -> Result<String, LocalVoiceError> {
            assert_eq!(model, DEFAULT_LOCAL_VOICE_MODEL);
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
        assert_eq!(info.local_model, DEFAULT_LOCAL_VOICE_MODEL);
        assert_eq!(info.local_models.len(), LOCAL_VOICE_MODELS.len());
        assert_eq!(info.local_models[0].state, "ready");
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("model.bin"));
        assert!(!json.contains("models/voice"));
        // The artifact identity is the runner's business, not the renderer's.
        assert!(!json.contains(LOCAL_VOICE_MODELS[0].sha256));
        assert!(!json.contains(LOCAL_VOICE_MODELS[0].file));
    }

    /// Every id must be unique and installable: the picker, the stored
    /// selection, and the runner's on-disk layout are all keyed by it.
    #[test]
    fn the_catalog_has_unique_ids_and_a_recommended_default() {
        let mut ids: Vec<&str> = LOCAL_VOICE_MODELS.iter().map(|model| model.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate local voice model id");
        let default = local_voice_model(DEFAULT_LOCAL_VOICE_MODEL).expect("default is in catalog");
        assert!(default.recommended);
        assert_eq!(
            LOCAL_VOICE_MODELS
                .iter()
                .filter(|model| model.recommended)
                .count(),
            1,
        );
        for model in LOCAL_VOICE_MODELS {
            assert_eq!(model.sha256.len(), 64, "{} digest", model.id);
            assert!(model.bytes > 0, "{} size", model.id);
        }
    }

    #[tokio::test]
    async fn an_unknown_local_model_is_refused_rather_than_stored() {
        let (_directory, store) = test_store().await;
        let secrets = TestSecrets::default();
        let error = update(
            &store,
            &secrets,
            &ReadyLocal,
            VoiceTranscriptionUpdate {
                model: VoiceTranscriptionModel::Local,
                local_model: Some("whisper-imaginary".into()),
            },
        )
        .await
        .expect_err("unknown model");
        assert!(format!("{error:?}").contains("unknown local voice model"));

        let stored = update(
            &store,
            &secrets,
            &ReadyLocal,
            VoiceTranscriptionUpdate {
                model: VoiceTranscriptionModel::Local,
                local_model: Some("base.en-q5_1".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(stored.local_model, "base.en-q5_1");
    }
}
