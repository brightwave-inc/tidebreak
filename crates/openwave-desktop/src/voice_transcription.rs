//! Trusted desktop-local voice transcription.
//!
//! The renderer sends only recorded bytes to the loopback API. This native
//! runner owns the pinned model path, download verification, media decoding,
//! resampling, and whisper.cpp inference.

use std::collections::HashMap;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use openwave_server::voice_transcription::{
    local_voice_model, LocalVoiceModel, LOCAL_VOICE_REPO_COMMIT,
};
use openwave_server::{LocalVoiceError, LocalVoiceRunner, LocalVoiceState, LocalVoiceStatus};
use sha2::{Digest, Sha256};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tokio::io::AsyncWriteExt as _;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;

fn model_version(model: &LocalVoiceModel) -> String {
    format!("whisper.cpp-{LOCAL_VOICE_REPO_COMMIT}-{}", model.id)
}

fn model_url(model: &LocalVoiceModel) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/{LOCAL_VOICE_REPO_COMMIT}/{}",
        model.file
    )
}

fn catalog_model(id: &str) -> Result<&'static LocalVoiceModel, String> {
    local_voice_model(id).ok_or_else(|| "Unknown local voice model".to_owned())
}

#[derive(Clone)]
pub(crate) struct DesktopLocalVoiceRunner {
    data_dir: PathBuf,
    /// Download progress and the last failure, per catalog model id. Only a
    /// model that has been downloaded in this session has an entry; an
    /// installed model's state is read from disk.
    state: Arc<Mutex<HashMap<&'static str, RuntimeState>>>,
    install_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
struct RuntimeState {
    downloading: bool,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
}

impl DesktopLocalVoiceRunner {
    pub(crate) fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            state: Arc::default(),
            install_lock: Arc::default(),
        }
    }

    fn model_dir(&self, model: &LocalVoiceModel) -> PathBuf {
        self.data_dir
            .join("models")
            .join("voice")
            .join(model_version(model))
    }

    fn model_path(&self, model: &LocalVoiceModel) -> PathBuf {
        self.model_dir(model).join("model.bin")
    }

    fn marker_path(&self, model: &LocalVoiceModel) -> PathBuf {
        self.model_dir(model).join("installed.json")
    }

    fn installed(&self, model: &LocalVoiceModel) -> bool {
        let Ok(marker) = std::fs::read(self.marker_path(model)) else {
            return false;
        };
        let Ok(marker): Result<serde_json::Value, _> = serde_json::from_slice(&marker) else {
            return false;
        };
        marker["version"].as_str() == Some(model_version(model).as_str())
            && marker["sha256"].as_str() == Some(model.sha256)
            && self.model_path(model).is_file()
    }

    fn status_now(&self, model: &LocalVoiceModel) -> LocalVoiceStatus {
        if self.installed(model) {
            return LocalVoiceStatus {
                state: LocalVoiceState::Ready,
                downloaded_bytes: Some(model.bytes),
                total_bytes: Some(model.bytes),
                error: None,
            };
        }
        let states = self.state.lock().expect("voice state lock");
        let Some(state) = states.get(model.id) else {
            return LocalVoiceStatus {
                state: LocalVoiceState::NotInstalled,
                downloaded_bytes: None,
                total_bytes: Some(model.bytes),
                error: None,
            };
        };
        LocalVoiceStatus {
            state: if state.downloading {
                LocalVoiceState::Downloading
            } else if state.error.is_some() {
                LocalVoiceState::Failed
            } else {
                LocalVoiceState::NotInstalled
            },
            downloaded_bytes: state.downloading.then_some(state.downloaded_bytes),
            total_bytes: state.total_bytes.or(Some(model.bytes)),
            error: state.error.clone(),
        }
    }

    async fn ensure_installed(&self, model: &LocalVoiceModel) -> Result<LocalVoiceStatus, String> {
        // One download at a time across the catalog: two multi-hundred-megabyte
        // fetches racing each other help nobody, and the picker starts one.
        let _guard = self.install_lock.lock().await;
        if self.installed(model) {
            return Ok(self.status_now(model));
        }
        {
            let mut states = self.state.lock().expect("voice state lock");
            let state = states.entry(model.id).or_default();
            state.downloading = true;
            state.downloaded_bytes = 0;
            state.total_bytes = Some(model.bytes);
            state.error = None;
        }
        let outcome = self.download_model(model).await;
        {
            let mut states = self.state.lock().expect("voice state lock");
            let state = states.entry(model.id).or_default();
            state.downloading = false;
            state.error = outcome.as_ref().err().cloned();
        }
        outcome.map(|()| self.status_now(model))
    }

    async fn download_model(&self, model: &LocalVoiceModel) -> Result<(), String> {
        let directory = self.model_dir(model);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| {
                format!("Could not prepare the local voice model directory: {error}")
            })?;
        let partial = directory.join("model.bin.partial");
        let _ = tokio::fs::remove_file(&partial).await;
        let response = reqwest::Client::new()
            .get(model_url(model))
            .send()
            .await
            .map_err(|_| "Could not download the local voice model".to_owned())?;
        if !response.status().is_success() {
            return Err(format!(
                "Could not download the local voice model: server answered {}",
                response.status()
            ));
        }
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|error| format!("Could not write the local voice model: {error}"))?;
        let mut stream = response.bytes_stream();
        let mut downloaded = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| "The local voice model download was interrupted".to_owned())?;
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("Could not write the local voice model: {error}"))?;
            downloaded += chunk.len() as u64;
            self.state
                .lock()
                .expect("voice state lock")
                .entry(model.id)
                .or_default()
                .downloaded_bytes = downloaded;
        }
        file.flush()
            .await
            .map_err(|error| format!("Could not write the local voice model: {error}"))?;
        drop(file);
        let digest = sha256_file(&partial)
            .map_err(|error| format!("Could not verify the local voice model: {error}"))?;
        if digest != model.sha256 {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err("The downloaded local voice model failed its pinned SHA-256 check and was discarded".into());
        }
        tokio::fs::rename(&partial, self.model_path(model))
            .await
            .map_err(|error| format!("Could not install the local voice model: {error}"))?;
        let marker = serde_json::to_vec_pretty(
            &serde_json::json!({"version": model_version(model), "sha256": model.sha256}),
        )
        .map_err(|_| "Could not record the local voice model install".to_owned())?;
        let marker_partial = directory.join("installed.json.partial");
        tokio::fs::write(&marker_partial, marker)
            .await
            .map_err(|error| format!("Could not record the local voice model install: {error}"))?;
        tokio::fs::rename(&marker_partial, self.marker_path(model))
            .await
            .map_err(|error| format!("Could not record the local voice model install: {error}"))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl LocalVoiceRunner for DesktopLocalVoiceRunner {
    async fn status(&self, model: &str) -> LocalVoiceStatus {
        match catalog_model(model) {
            Ok(model) => self.status_now(model),
            Err(error) => LocalVoiceStatus {
                state: LocalVoiceState::Unavailable,
                downloaded_bytes: None,
                total_bytes: None,
                error: Some(error),
            },
        }
    }

    async fn install(&self, model: &str) -> Result<LocalVoiceStatus, String> {
        self.ensure_installed(catalog_model(model)?).await
    }

    async fn transcribe(
        &self,
        model: &str,
        content_type: &str,
        audio: Vec<u8>,
    ) -> Result<String, LocalVoiceError> {
        let model = catalog_model(model).map_err(LocalVoiceError::Runner)?;
        self.ensure_installed(model)
            .await
            .map_err(LocalVoiceError::Runner)?;
        let path = self.model_path(model);
        let language = if model.english_only { "en" } else { "auto" };
        let content_type = content_type.to_owned();
        tokio::task::spawn_blocking(move || {
            transcribe_blocking(&path, language, &content_type, audio)
        })
        .await
        .map_err(|_| {
            LocalVoiceError::Runner(
                "Local voice transcription worker stopped unexpectedly".to_owned(),
            )
        })?
    }
}

fn transcribe_blocking(
    model: &Path,
    language: &str,
    content_type: &str,
    audio: Vec<u8>,
) -> Result<String, LocalVoiceError> {
    let samples = decode_audio(content_type, audio)?;
    let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|_| LocalVoiceError::Runner("Could not load the local voice model".to_owned()))?;
    let mut state = context.create_state().map_err(|_| {
        LocalVoiceError::Runner("Could not initialize local voice transcription".to_owned())
    })?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    // "auto" asks whisper to detect the language; the English-only models
    // reject anything else, so the catalog decides which is passed.
    params.set_language(Some(language));
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, &samples)
        .map_err(|_| LocalVoiceError::Runner("Local voice transcription failed".to_owned()))?;
    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().map_err(|_| {
            LocalVoiceError::Runner("Local voice transcription returned invalid text".to_owned())
        })?);
    }
    Ok(text.trim().to_owned())
}

fn decode_audio(content_type: &str, audio: Vec<u8>) -> Result<Vec<f32>, LocalVoiceError> {
    let undecodable =
        |message: &str| -> LocalVoiceError { LocalVoiceError::Undecodable(message.to_owned()) };
    let mime = content_type.split(';').next().unwrap_or("");
    if !matches!(mime, "audio/webm" | "audio/mp4") {
        return Err(LocalVoiceError::UnsupportedMedia(
            "Unsupported voice recording format".into(),
        ));
    }
    let mut hint = Hint::new();
    hint.with_extension(if mime == "audio/mp4" { "m4a" } else { "webm" });
    let source = MediaSourceStream::new(Box::new(Cursor::new(audio)), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| undecodable("Could not read the voice recording container"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| undecodable("The voice recording has no audio track"))?;
    let track_id = track.id;
    let CodecParameters::Audio(params) = track
        .codec_params
        .as_ref()
        .ok_or_else(|| undecodable("The voice recording has no audio codec"))?
    else {
        return Err(undecodable("The voice recording has no audio codec"));
    };
    let rate = params
        .sample_rate
        .ok_or_else(|| undecodable("The voice recording has no sample rate"))?;
    let channels = params
        .channels
        .as_ref()
        .map(|channels| channels.count())
        .ok_or_else(|| undecodable("The voice recording has no channel layout"))?;
    if !(1..=2).contains(&channels) {
        return Err(undecodable("Voice recordings must be mono or stereo"));
    }
    let mut interleaved = Vec::new();
    if params.codec.to_string() == "0x1001" {
        let mut decoder = opus_decoder::OpusDecoder::new(rate, channels)
            .map_err(|_| LocalVoiceError::Runner("Could not initialize the Opus decoder".into()))?;
        let mut pcm = vec![0_i16; decoder.max_frame_size_per_channel() * channels];
        loop {
            match format.next_packet() {
                Ok(Some(packet)) if packet.track_id == track_id => {
                    let frames = decoder
                        .decode(&packet.data, &mut pcm, false)
                        .map_err(|_| undecodable("Could not decode the WebM/Opus recording"))?;
                    interleaved.extend(
                        pcm[..frames * channels]
                            .iter()
                            .map(|sample| *sample as f32 / i16::MAX as f32),
                    );
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(SymphoniaError::IoError(_)) => break,
                Err(_) => return Err(undecodable("Could not read the WebM/Opus recording")),
            }
        }
    } else {
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|_| {
                LocalVoiceError::UnsupportedMedia("Unsupported codec in voice recording".into())
            })?;
        loop {
            match format.next_packet() {
                Ok(Some(packet)) if packet.track_id == track_id => match decoder.decode(&packet) {
                    Ok(buffer) => {
                        let start = interleaved.len();
                        interleaved.resize(start + buffer.samples_interleaved(), 0.0);
                        buffer.copy_to_slice_interleaved(&mut interleaved[start..]);
                    }
                    Err(SymphoniaError::DecodeError(_)) => {}
                    Err(_) => return Err(undecodable("Could not decode the MP4 recording")),
                },
                Ok(Some(_)) => {}
                Ok(None) | Err(SymphoniaError::IoError(_)) => break,
                Err(_) => return Err(undecodable("Could not read the MP4 recording")),
            }
        }
    }
    if interleaved.is_empty() {
        return Err(undecodable(
            "The voice recording contained no decodable audio",
        ));
    }
    let mono: Vec<f32> = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks_exact(2)
            .map(|pair| (pair[0] + pair[1]) * 0.5)
            .collect()
    };
    Ok(resample_linear(&mono, rate, WHISPER_SAMPLE_RATE))
}

fn resample_linear(input: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate {
        return input.to_vec();
    }
    let output_len = ((input.len() as u64 * target_rate as u64) / source_rate as u64) as usize;
    let ratio = source_rate as f64 / target_rate as f64;
    (0..output_len)
        .map(|index| {
            let position = index as f64 * ratio;
            let left = position.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (position - left as f64) as f32;
            input[left] + (input[right] - input[left]) * fraction
        })
        .collect()
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_preserves_endpoints_and_expected_length() {
        let output = resample_linear(&[0.0, 1.0, 0.0, -1.0], 4, 8);
        assert_eq!(output.len(), 8);
        assert_eq!(output[0], 0.0);
    }

    #[test]
    fn marker_must_match_the_exact_pinned_model() {
        let dir = tempfile::tempdir().unwrap();
        let runner = DesktopLocalVoiceRunner::new(dir.path().to_owned());
        let model = catalog_model("tiny.en-q5_1").unwrap();
        std::fs::create_dir_all(runner.model_dir(model)).unwrap();
        std::fs::write(runner.model_path(model), b"model").unwrap();
        std::fs::write(
            runner.marker_path(model),
            br#"{"version":"other","sha256":"bad"}"#,
        )
        .unwrap();
        assert!(!runner.installed(model));
    }

    /// Two catalog entries must never share an install directory, or one
    /// download would silently satisfy the other's verified marker.
    #[test]
    fn every_catalog_model_installs_to_its_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let runner = DesktopLocalVoiceRunner::new(dir.path().to_owned());
        let mut paths: Vec<PathBuf> = openwave_server::voice_transcription::LOCAL_VOICE_MODELS
            .iter()
            .map(|model| runner.model_path(model))
            .collect();
        let count = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), count);
    }

    #[test]
    fn decodes_a_recording_whose_clusters_have_an_unknown_size() {
        // A browser's MediaRecorder muxes into a non-seekable sink, so neither
        // the Segment nor any Cluster length is known when its header is
        // written. The fixture is a 1.5s 48 kHz mono Opus recording rewritten
        // into exactly that shape: every Cluster carries the unknown-size vint
        // and the seek-oriented elements are gone. Reading it used to stop at
        // the first cluster boundary with "Could not read the WebM/Opus
        // recording".
        let samples = decode_audio(
            "audio/webm;codecs=opus",
            include_bytes!("../tests/fixtures/mediarecorder-opus.webm").to_vec(),
        )
        .expect("decodes live-muxed WebM/Opus");
        let seconds = samples.len() as f32 / WHISPER_SAMPLE_RATE as f32;
        assert!(seconds > 1.4, "decoded only {seconds}s of the recording");
    }

    /// Drives a second catalog entry through the real install path: download,
    /// SHA-256 verification, marker, and inference on a recording. Ignored
    /// because it fetches 57 MB; run it when the catalog or the runner's
    /// per-model layout changes.
    ///
    /// `cargo test -p openwave-desktop installs_and_transcribes -- --ignored`
    #[tokio::test]
    #[ignore = "downloads a catalog model over the network"]
    async fn installs_and_transcribes_with_a_second_catalog_model() {
        let dir = tempfile::tempdir().unwrap();
        let runner = DesktopLocalVoiceRunner::new(dir.path().to_owned());
        let id = "base.en-q5_1";
        let model = catalog_model(id).unwrap();

        assert_eq!(runner.status(id).await.state, LocalVoiceState::NotInstalled);
        let installed = runner.install(id).await.expect("install");
        assert_eq!(installed.state, LocalVoiceState::Ready);
        // Verification is the install's contract: the file on disk is exactly
        // the pinned artifact, not merely something the server answered with.
        assert_eq!(
            sha256_file(&runner.model_path(model)).unwrap(),
            model.sha256
        );
        assert_eq!(
            std::fs::metadata(runner.model_path(model)).unwrap().len(),
            model.bytes
        );
        assert_eq!(runner.status(id).await.state, LocalVoiceState::Ready);
        // A second install is a no-op rather than a second download.
        assert_eq!(
            runner.install(id).await.expect("reinstall").state,
            LocalVoiceState::Ready
        );

        // The fixture is a tone, not speech, so the transcript is expected to
        // be empty. What this proves is the plumbing: the id selected a model,
        // its verified bytes loaded, and inference ran to completion on a
        // recording decoded by the same path the app uses.
        runner
            .transcribe(
                id,
                "audio/webm;codecs=opus",
                include_bytes!("../tests/fixtures/mediarecorder-opus.webm").to_vec(),
            )
            .await
            .expect("runs inference with the selected model");

        // The id names the model: an entry that is not in the catalog cannot
        // silently fall back to one that is.
        assert!(runner.install("whisper-imaginary").await.is_err());
    }

    #[test]
    #[ignore = "downloads the pinned model and runs real local inference"]
    fn transcribes_real_webm_and_mp4_fixtures() {
        let webm = std::env::var("OPENWAVE_TEST_VOICE_WEBM").expect("webm fixture path");
        let mp4 = std::env::var("OPENWAVE_TEST_VOICE_MP4").expect("mp4 fixture path");
        let model = std::env::var("OPENWAVE_TEST_VOICE_MODEL").expect("model path");
        for (mime, path) in [("audio/webm", webm), ("audio/mp4", mp4)] {
            let text = transcribe_blocking(
                Path::new(&model),
                "en",
                mime,
                std::fs::read(path).expect("voice fixture"),
            )
            .expect("local transcription");
            assert!(!text.is_empty());
        }
    }
}
