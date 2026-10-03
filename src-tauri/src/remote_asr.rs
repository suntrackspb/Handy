//! Remote ASR: send the recording to a self-hosted PersonalWhisper server
//! (`POST /dictate`: faster-whisper + LLM cleanup) instead of a local model.
//!
//! Kept in one file on purpose so the fork stays easy to rebase on upstream.
//! The server returns finished text, so the local post-processing is not needed.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;

use crate::settings::{get_settings, write_settings};

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
#[serde(default)]
pub struct RemoteAsrSettings {
    pub enabled: bool,
    /// Base URL, e.g. `http://192.168.1.20:8765`.
    pub url: String,
    pub token: String,
    /// `clean` (Whisper + LLM) or `raw` (Whisper only).
    pub mode: String,
}

impl Default for RemoteAsrSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            token: String::new(),
            mode: "clean".to_string(),
        }
    }
}

#[derive(Serialize, Debug, Clone, Default, Type)]
pub struct RemoteAsrStatus {
    pub reachable: bool,
    pub whisper_loaded: bool,
    pub llm_loaded: bool,
    pub llm: String,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct HealthResponse {
    #[serde(default)]
    whisper_loaded: bool,
    #[serde(default)]
    llm_loaded: bool,
    #[serde(default)]
    llm: String,
}

#[derive(Deserialize)]
struct DictateResponse {
    text: String,
    #[serde(default)]
    llm_used: bool,
    #[serde(default)]
    fallback_reason: Option<String>,
    #[serde(default)]
    timings: serde_json::Value,
}

fn base_url(s: &RemoteAsrSettings) -> Result<String> {
    let url = s.url.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err(anyhow!("Remote ASR URL is not set"));
    }
    Ok(url.to_string())
}

/// 16 kHz mono 16-bit WAV in memory (same format Handy saves to history).
fn encode_wav(samples: &[f32]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::with_capacity(samples.len() * 2 + 64));
    {
        let mut writer = hound::WavWriter::new(&mut buf, spec)?;
        for s in samples {
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
    }
    Ok(buf.into_inner())
}

/// The server accepts `ru`, `en` and `auto`; anything else is left to Whisper to detect.
fn server_language(selected: &str) -> &'static str {
    match selected.split(['-', '_']).next().unwrap_or("") {
        "ru" => "ru",
        "en" => "en",
        _ => "auto",
    }
}

/// Blocking call used from `TranscriptionManager::transcribe`, which runs on a worker
/// that may sit inside an async runtime, so the request goes through a plain thread.
pub fn transcribe(
    s: &RemoteAsrSettings,
    custom_words: &[String],
    selected_language: &str,
    samples: &[f32],
) -> Result<String> {
    let url = format!("{}/dictate", base_url(s)?);
    let wav = encode_wav(samples)?;
    let token = s.token.clone();
    let mode = if s.mode == "raw" { "raw" } else { "clean" }.to_string();
    let language = server_language(selected_language).to_string();
    let words = custom_words.join(",");

    let handle = std::thread::spawn(move || -> Result<DictateResponse> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .build()?;
        let form = reqwest::blocking::multipart::Form::new()
            .part(
                "audio",
                reqwest::blocking::multipart::Part::bytes(wav)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            )
            .text("language", language)
            .text("mode", mode)
            .text("words", words);
        let resp = client
            .post(&url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .context("Remote ASR server is unreachable")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("Remote ASR server returned {}: {}", status, body));
        }
        Ok(resp.json::<DictateResponse>()?)
    });
    let r = handle
        .join()
        .map_err(|_| anyhow!("Remote ASR request thread panicked"))??;

    info!(
        "Remote ASR: llm_used={} fallback={:?} timings={}",
        r.llm_used, r.fallback_reason, r.timings
    );
    Ok(r.text)
}

fn authed(
    client: &reqwest::Client,
    method: reqwest::Method,
    s: &RemoteAsrSettings,
    path: &str,
) -> Result<reqwest::RequestBuilder> {
    Ok(client
        .request(method, format!("{}{}", base_url(s)?, path))
        .bearer_auth(&s.token))
}

async fn fetch_status(s: &RemoteAsrSettings) -> RemoteAsrStatus {
    let result: Result<HealthResponse> = async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()?;
        let resp = client
            .get(format!("{}/health", base_url(s)?))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }
    .await;
    match result {
        Ok(h) => RemoteAsrStatus {
            reachable: true,
            whisper_loaded: h.whisper_loaded,
            llm_loaded: h.llm_loaded,
            llm: h.llm,
            error: None,
        },
        Err(e) => RemoteAsrStatus {
            error: Some(e.to_string()),
            ..Default::default()
        },
    }
}

/// `POST /warmup` or `/unload`, then report the fresh status.
async fn control(s: &RemoteAsrSettings, path: &str) -> Result<RemoteAsrStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(200)) // a cold LLM load takes ~20 s
        .build()
        .map_err(|e| e.to_string())?;
    let resp = authed(&client, reqwest::Method::POST, s, path)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        warn!("Remote ASR {} failed: {}", path, resp.status());
        return Err(format!("Server returned {}", resp.status()));
    }
    Ok(fetch_status(s).await)
}

#[tauri::command]
#[specta::specta]
pub fn set_remote_asr_settings(app: AppHandle, remote_asr: RemoteAsrSettings) {
    let mut settings = get_settings(&app);
    if remote_asr.enabled {
        // No local model will ever be picked, so don't send the user back to onboarding.
        settings.onboarding_completed = true;
    }
    settings.remote_asr = remote_asr;
    write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub async fn remote_asr_status(app: AppHandle) -> RemoteAsrStatus {
    fetch_status(&get_settings(&app).remote_asr).await
}

#[tauri::command]
#[specta::specta]
pub async fn remote_asr_warmup(app: AppHandle) -> Result<RemoteAsrStatus, String> {
    control(&get_settings(&app).remote_asr, "/warmup").await
}

#[tauri::command]
#[specta::specta]
pub async fn remote_asr_unload(app: AppHandle) -> Result<RemoteAsrStatus, String> {
    control(&get_settings(&app).remote_asr, "/unload").await
}
