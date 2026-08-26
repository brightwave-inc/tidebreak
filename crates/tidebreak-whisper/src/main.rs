//! One-shot whisper.cpp transcription helper.
//!
//! The desktop app spawns this binary per transcription instead of linking
//! whisper.cpp, so the C++ build stays out of the app, its release pipeline,
//! and its caches. The contract is deliberately tiny:
//!
//! - arguments: `--model <path> --language <en|auto>`
//! - stdin: mono 16 kHz PCM as little-endian `f32` samples
//! - stdout: the transcript, UTF-8, no trailing newline
//! - exit: `0` on success; non-zero with a reason on stderr otherwise
//!
//! There is no protocol negotiation. The desktop pins the exact published
//! helper version and verifies its updater signature before the first spawn,
//! so both sides of this contract always come from the same source revision.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A hard ceiling on stdin, comfortably above any real recording: one hour of
/// 16 kHz mono f32 audio is about 230 MB. The desktop is the only caller, so
/// this only guards against a runaway pipe, not a hostile one.
const MAX_STDIN_BYTES: u64 = 1024 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let (model, language) = parse_args()?;
    let mut raw = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_STDIN_BYTES)
        .read_to_end(&mut raw)
        .map_err(|error| format!("could not read audio samples from stdin: {error}"))?;
    if raw.len() % 4 != 0 {
        return Err("stdin did not contain whole little-endian f32 samples".to_owned());
    }
    if raw.is_empty() {
        return Err("stdin contained no audio samples".to_owned());
    }
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();

    let context = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
        .map_err(|error| format!("could not load the voice model: {error}"))?;
    let mut state = context
        .create_state()
        .map_err(|error| format!("could not initialize transcription: {error}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    // "auto" asks whisper to detect the language; the English-only models
    // reject anything else, so the caller's catalog decides which is passed.
    params.set_language(Some(&language));
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, &samples)
        .map_err(|error| format!("transcription failed: {error}"))?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(
            &segment
                .to_str_lossy()
                .map_err(|error| format!("transcription returned invalid text: {error}"))?,
        );
    }
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.trim().as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("could not write the transcript: {error}"))
}

fn parse_args() -> Result<(PathBuf, String), String> {
    let mut model = None;
    let mut language = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match flag.as_str() {
            "--model" => model = Some(PathBuf::from(value("--model")?)),
            "--language" => language = Some(value("--language")?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((
        model.ok_or("--model is required")?,
        language.ok_or("--language is required")?,
    ))
}
