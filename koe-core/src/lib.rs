pub mod asr;
pub mod audio_buffer;
pub mod config;
pub mod dictionary;
pub mod errors;
pub mod ffi;
pub mod llm;
pub mod prompt;
pub mod session;
pub mod telemetry;
pub mod transcript;

use crate::asr::doubao_ws::DoubaoWsProvider;
use crate::asr::{AsrConfig, AsrEvent, AsrProvider};
use crate::config::Config;
use crate::ffi::{
    cstr_to_str, invoke_final_text_ready, invoke_session_error, invoke_session_ready,
    invoke_session_warning, invoke_state_changed, invoke_uncertain_phrases_ready, SPCallbacks,
    SPFeedbackConfig, SPHotkeyConfig, SPSessionContext, SPSessionMode,
};
use crate::llm::openai_compatible::OpenAiCompatibleProvider;
use crate::llm::{CorrectionRequest, LlmProvider};
use crate::session::{Session, SessionState};
use crate::transcript::TranscriptAggregator;

use serde::{Deserialize, Serialize};
use std::ffi::c_char;
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// Global core state
struct Core {
    runtime: Runtime,
    audio_tx: Option<mpsc::Sender<Vec<u8>>>,
    session: Arc<Mutex<Option<Session>>>,
    config: Config,
    dictionary: Vec<String>,
    system_prompt: String,
    user_prompt_template: String,
}

static CORE: Mutex<Option<Core>> = Mutex::new(None);

#[derive(Debug, Serialize)]
struct UncertainPhraseCandidate {
    session_id: String,
    phrase: String,
    suggestion: String,
    reason: String,
    confidence: f64,
    context_text: String,
}

#[derive(Debug, Deserialize)]
struct UncertainPhraseCandidateRaw {
    phrase: Option<String>,
    suggestion: Option<String>,
    reason: Option<String>,
    confidence: Option<f64>,
    context_text: Option<String>,
}

// ─── FFI Entry Points ───────────────────────────────────────────────

/// Initialize the core. Must be called once before any other function.
/// `config_path` is reserved for future use (currently loads from ~/.koe/config.yaml).
#[no_mangle]
pub extern "C" fn sp_core_create(config_path: *const c_char) -> i32 {
    telemetry::init_logging();

    let _config_path = unsafe { cstr_to_str(config_path) };
    log::info!("sp_core_create called");

    // Ensure ~/.koe/ exists with default config and dictionary
    match config::ensure_defaults() {
        Ok(true) => log::info!("created default config files in ~/.koe/"),
        Ok(false) => {}
        Err(e) => log::warn!("ensure_defaults failed: {e}"),
    }

    // Load config
    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("failed to load config, using defaults: {e}");
            Config::default()
        }
    };

    // Load dictionary
    let dict_path = config::resolve_dictionary_path(&cfg);
    let dictionary = match dictionary::load_dictionary(&dict_path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("failed to load dictionary: {e}");
            vec![]
        }
    };

    // Load prompts
    let system_prompt = prompt::load_system_prompt(&config::resolve_system_prompt_path(&cfg));
    let user_prompt_template = prompt::load_user_prompt_template(&config::resolve_user_prompt_path(&cfg));

    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log::error!("failed to create tokio runtime: {e}");
            return -1;
        }
    };

    let core = Core {
        runtime,
        audio_tx: None,
        session: Arc::new(Mutex::new(None)),
        config: cfg,
        dictionary,
        system_prompt,
        user_prompt_template,
    };

    let mut global = CORE.lock().unwrap();
    *global = Some(core);

    log::info!("core initialized");
    0
}

/// Shut down the core and release all resources.
#[no_mangle]
pub extern "C" fn sp_core_destroy() {
    log::info!("sp_core_destroy called");
    let mut global = CORE.lock().unwrap();
    *global = None;
}

/// Register callbacks from the Obj-C side.
#[no_mangle]
pub extern "C" fn sp_core_register_callbacks(callbacks: SPCallbacks) {
    ffi::register_callbacks(callbacks);
}

/// Reload configuration and dictionary from disk.
/// Takes effect on the next session.
#[no_mangle]
pub extern "C" fn sp_core_reload_config() -> i32 {
    log::info!("sp_core_reload_config called");

    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            log::error!("reload config failed: {e}");
            return -1;
        }
    };

    let dict_path = config::resolve_dictionary_path(&cfg);
    let dictionary = match dictionary::load_dictionary(&dict_path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("reload dictionary failed: {e}");
            vec![]
        }
    };

    let system_prompt = prompt::load_system_prompt(&config::resolve_system_prompt_path(&cfg));
    let user_prompt_template = prompt::load_user_prompt_template(&config::resolve_user_prompt_path(&cfg));

    let mut global = CORE.lock().unwrap();
    if let Some(ref mut core) = *global {
        core.config = cfg;
        core.dictionary = dictionary;
        core.system_prompt = system_prompt;
        core.user_prompt_template = user_prompt_template;
        log::info!("config, dictionary, and prompts reloaded");
    }

    0
}

/// Begin a new voice input session.
#[no_mangle]
pub extern "C" fn sp_core_session_begin(context: SPSessionContext) -> i32 {
    let bundle_id = unsafe { cstr_to_str(context.frontmost_bundle_id) }.map(|s| s.to_string());

    log::info!(
        "sp_core_session_begin: mode={:?}, app={:?}, pid={}",
        context.mode,
        bundle_id,
        context.frontmost_pid,
    );

    let mut global = CORE.lock().unwrap();
    let core = match global.as_mut() {
        Some(c) => c,
        None => {
            log::error!("core not initialized");
            return -1;
        }
    };

    // Hot-reload: re-read config, dictionary, and prompts at session start
    // Files are tiny so overhead is negligible — no need to manually Reload Config
    if let Ok(new_cfg) = config::load_config() {
        let dict_path = config::resolve_dictionary_path(&new_cfg);
        if let Ok(d) = dictionary::load_dictionary(&dict_path) {
            core.dictionary = d;
        }
        core.system_prompt = prompt::load_system_prompt(&config::resolve_system_prompt_path(&new_cfg));
        core.user_prompt_template = prompt::load_user_prompt_template(&config::resolve_user_prompt_path(&new_cfg));
        core.config = new_cfg;
    }

    // Create session
    let session = Session::new(context.mode, bundle_id, context.frontmost_pid);
    let session_id = session.id.clone();
    let mode = context.mode;

    // Audio channel
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(1024);
    core.audio_tx = Some(audio_tx);

    let session_arc = core.session.clone();
    {
        let mut s = session_arc.lock().unwrap();
        *s = Some(session);
    }

    // Capture config for the async task
    let cfg = &core.config;
    let asr_config = AsrConfig {
        url: cfg.asr.url.clone(),
        app_key: cfg.asr.app_key.clone(),
        access_key: cfg.asr.access_key.clone(),
        resource_id: cfg.asr.resource_id.clone(),
        sample_rate_hz: 16000,
        connect_timeout_ms: cfg.asr.connect_timeout_ms,
        final_wait_timeout_ms: cfg.asr.final_wait_timeout_ms,
        enable_ddc: cfg.asr.enable_ddc,
        enable_itn: cfg.asr.enable_itn,
        enable_punc: cfg.asr.enable_punc,
        enable_nonstream: cfg.asr.enable_nonstream,
        hotwords: core.dictionary.clone(),
    };
    let llm_config = cfg.llm.clone();
    let dictionary = core.dictionary.clone();
    let dictionary_max_candidates = cfg.llm.dictionary_max_candidates;
    let system_prompt = core.system_prompt.clone();
    let user_prompt_template = core.user_prompt_template.clone();

    // Spawn the session task
    core.runtime.spawn(async move {
        run_session(
            session_arc,
            session_id,
            mode,
            audio_rx,
            asr_config,
            llm_config,
            dictionary,
            dictionary_max_candidates,
            system_prompt,
            user_prompt_template,
        )
        .await;
    });

    0
}

/// Push an audio frame into the current session.
#[no_mangle]
pub extern "C" fn sp_core_push_audio(
    frame: *const u8,
    len: u32,
    _timestamp: u64,
) -> i32 {
    if frame.is_null() || len == 0 {
        return -1;
    }

    let data = unsafe { std::slice::from_raw_parts(frame, len as usize) }.to_vec();

    let global = CORE.lock().unwrap();
    if let Some(ref core) = *global {
        if let Some(ref tx) = core.audio_tx {
            if tx.try_send(data).is_err() {
                log::warn!("audio channel full, frame dropped");
            }
        }
    }
    0
}

/// End the current session (user released hotkey or tapped again).
#[no_mangle]
pub extern "C" fn sp_core_session_end() -> i32 {
    log::info!("sp_core_session_end called");

    let mut global = CORE.lock().unwrap();
    if let Some(ref mut core) = *global {
        // Drop the audio sender to signal the session task
        core.audio_tx = None;
    }
    0
}

/// Query current feedback configuration.
#[no_mangle]
pub extern "C" fn sp_core_get_feedback_config() -> SPFeedbackConfig {
    let global = CORE.lock().unwrap();
    if let Some(ref core) = *global {
        SPFeedbackConfig {
            start_sound: core.config.feedback.start_sound,
            stop_sound: core.config.feedback.stop_sound,
            error_sound: core.config.feedback.error_sound,
        }
    } else {
        SPFeedbackConfig {
            start_sound: true,
            stop_sound: true,
            error_sound: true,
        }
    }
}

/// Query current hotkey configuration.
/// Returns key codes and modifier flags for the configured trigger key.
/// If not configured, defaults to Fn key (keyCode 63/179).
#[no_mangle]
pub extern "C" fn sp_core_get_hotkey_config() -> SPHotkeyConfig {
    let global = CORE.lock().unwrap();
    if let Some(ref core) = *global {
        let params = core.config.hotkey.resolve();
        SPHotkeyConfig {
            key_code: params.key_code,
            alt_key_code: params.alt_key_code,
            modifier_flag: params.modifier_flag,
        }
    } else {
        // Default to Fn key
        SPHotkeyConfig {
            key_code: 63,
            alt_key_code: 179,
            modifier_flag: 0x00800000,
        }
    }
}

// ─── Session Task ───────────────────────────────────────────────────

async fn run_session(
    session_arc: Arc<Mutex<Option<Session>>>,
    session_id: String,
    mode: SPSessionMode,
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    asr_config: AsrConfig,
    llm_config: config::LlmSection,
    dictionary: Vec<String>,
    dictionary_max_candidates: usize,
    system_prompt: String,
    user_prompt_template: String,
) {
    let final_wait_timeout_ms = asr_config.final_wait_timeout_ms;

    // --- Connect ASR ---
    invoke_state_changed("connecting_asr");
    let mut asr = DoubaoWsProvider::new();
    if let Err(e) = asr.connect(&asr_config).await {
        log::error!("[{session_id}] ASR connection failed: {e}");
        invoke_session_error(&e.to_string());
        invoke_state_changed("failed");
        cleanup_session(&session_arc);
        return;
    }

    // Transition to recording
    let recording_state = match mode {
        SPSessionMode::Hold => SessionState::RecordingHold,
        SPSessionMode::Toggle => SessionState::RecordingToggle,
    };
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            let _ = session.transition(recording_state);
        }
    }
    invoke_state_changed(&recording_state.to_string());
    invoke_session_ready();

    // --- Stream audio to ASR + collect results ---
    let mut aggregator = TranscriptAggregator::new();
    let mut asr_done = false;

    // Stream audio frames until the channel is closed (session_end drops the sender)
    loop {
        tokio::select! {
            frame = audio_rx.recv() => {
                match frame {
                    Some(data) => {
                        if let Err(e) = asr.send_audio(&data).await {
                            log::error!("[{session_id}] ASR send error: {e}");
                            break;
                        }
                    }
                    None => {
                        // Channel closed: session ended
                        log::info!("[{session_id}] audio stream ended, sending finish");
                        let _ = asr.finish_input().await;
                        break;
                    }
                }
            }
            event = asr.next_event() => {
                match event {
                    Ok(AsrEvent::Interim(text)) => {
                        if !text.is_empty() {
                            aggregator.update_interim(&text);
                        }
                    }
                    Ok(AsrEvent::Definite(text)) => {
                        aggregator.update_definite(&text);
                    }
                    Ok(AsrEvent::Final(text)) => {
                        aggregator.update_final(&text);
                    }
                    Ok(AsrEvent::Closed) => {
                        asr_done = true;
                        break;
                    }
                    Ok(AsrEvent::Error(msg)) => {
                        log::error!("[{session_id}] ASR error event: {msg}");
                    }
                    Ok(AsrEvent::Connected) => {}
                    Err(e) => {
                        log::error!("[{session_id}] ASR read error: {e}");
                        break;
                    }
                }
            }
        }
    }

    // --- Finalize ASR ---
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            let _ = session.transition(SessionState::FinalizingAsr);
        }
    }
    invoke_state_changed("finalizing_asr");

    // Wait for final result if we haven't received one yet
    if !aggregator.has_final_result() && !asr_done {
        let wait_result = timeout(
            Duration::from_millis(final_wait_timeout_ms),
            wait_for_final(&mut asr, &mut aggregator),
        )
        .await;

        if wait_result.is_err() {
            log::warn!("[{session_id}] ASR final result timed out");
        }
    }

    let _ = asr.close().await;

    let asr_text = aggregator.best_text().to_string();
    if asr_text.is_empty() {
        log::warn!("[{session_id}] no ASR text available");
        invoke_session_error("no speech recognized");
        invoke_state_changed("failed");
        cleanup_session(&session_arc);
        return;
    }

    let interim_history = aggregator.interim_history(10).to_vec();
    log::info!(
        "[{session_id}] ASR result: {} chars, {} interim revisions",
        asr_text.len(),
        interim_history.len(),
    );

    // Store ASR text in session
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            session.asr_text = Some(asr_text.clone());
        }
    }

    // --- LLM Correction ---
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            let _ = session.transition(SessionState::Correcting);
        }
    }
    invoke_state_changed("correcting");
    let llm_config_for_uncertain = llm_config.clone();

    let final_text = if !llm_config.base_url.is_empty() && !llm_config.api_key.is_empty() {
        let llm = OpenAiCompatibleProvider::new(
            llm_config.base_url,
            llm_config.api_key,
            llm_config.model,
            llm_config.temperature,
            llm_config.top_p,
            llm_config.max_output_tokens,
            llm_config.timeout_ms,
        );

        // Filter dictionary candidates for prompt
        let candidates = prompt::filter_dictionary_candidates(
            &dictionary,
            &asr_text,
            dictionary_max_candidates,
        );

        log::info!("[{session_id}] LLM request — asr_text: \"{}\"", asr_text);
        log::info!("[{session_id}] LLM request — {} dictionary entries, {} interim revisions",
            candidates.len(), interim_history.len());

        let user_prompt = prompt::render_user_prompt(&user_prompt_template, &asr_text, &candidates, &interim_history);
        log::debug!("[{session_id}] LLM user prompt:\n{}", user_prompt);

        let request = CorrectionRequest {
            asr_text: asr_text.clone(),
            dictionary_entries: candidates,
            system_prompt,
            user_prompt,
        };

        match llm.correct(&request).await {
            Ok(corrected) => {
                log::info!("[{session_id}] LLM corrected: {} chars", corrected.len());
                corrected
            }
            Err(e) => {
                log::warn!("[{session_id}] LLM failed, falling back to ASR text: {e}");
                invoke_session_warning(&format!("LLM correction failed: {e}"));
                asr_text.clone()
            }
        }
    } else {
        log::info!("[{session_id}] LLM not configured, using raw ASR text");
        asr_text.clone()
    };

    // Store corrected text
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            session.corrected_text = Some(final_text.clone());
            let _ = session.transition(SessionState::PreparingPaste);
        }
    }
    invoke_state_changed("preparing_paste");

    // --- Deliver result to Obj-C ---
    invoke_final_text_ready(&final_text);

    // --- Optional uncertainty extraction (async, non-blocking for paste path) ---
    if !llm_config_for_uncertain.base_url.is_empty()
        && !llm_config_for_uncertain.api_key.is_empty()
        && !interim_history.is_empty()
    {
        let session_id_for_uncertain = session_id.clone();
        let asr_for_uncertain = asr_text.clone();
        let final_for_uncertain = final_text.clone();
        let interim_for_uncertain = interim_history.clone();

        tokio::spawn(async move {
            let candidates = extract_uncertain_phrases_with_llm(
                &session_id_for_uncertain,
                &llm_config_for_uncertain,
                &asr_for_uncertain,
                &final_for_uncertain,
                &interim_for_uncertain,
            )
            .await;

            if candidates.is_empty() {
                return;
            }

            match serde_json::to_string(&candidates) {
                Ok(payload) => {
                    log::info!(
                        "[{session_id_for_uncertain}] uncertainty candidates: {}",
                        candidates.len()
                    );
                    invoke_uncertain_phrases_ready(&payload);
                }
                Err(e) => {
                    log::warn!(
                        "[{session_id_for_uncertain}] failed to serialize uncertainty payload: {e}"
                    );
                }
            }
        });
    }

    // Session complete
    {
        let mut s = session_arc.lock().unwrap();
        if let Some(ref mut session) = *s {
            let _ = session.transition(SessionState::Pasting);
            // Pasting and clipboard restore happen on the Obj-C side
            // We transition directly to Completed here
            let _ = session.transition(SessionState::Completed);
        }
    }
    invoke_state_changed("completed");

    log::info!("[{session_id}] session completed");
    cleanup_session(&session_arc);
    invoke_state_changed("idle");
}

async fn extract_uncertain_phrases_with_llm(
    session_id: &str,
    llm_config: &config::LlmSection,
    asr_text: &str,
    final_text: &str,
    interim_history: &[String],
) -> Vec<UncertainPhraseCandidate> {
    let llm = OpenAiCompatibleProvider::new(
        llm_config.base_url.clone(),
        llm_config.api_key.clone(),
        llm_config.model.clone(),
        llm_config.temperature,
        llm_config.top_p,
        llm_config.max_output_tokens,
        llm_config.timeout_ms,
    );

    let request = CorrectionRequest {
        asr_text: asr_text.to_string(),
        dictionary_entries: vec![],
        system_prompt: build_uncertain_system_prompt().to_string(),
        user_prompt: build_uncertain_user_prompt(asr_text, final_text, interim_history),
    };

    let raw = match llm.correct(&request).await {
        Ok(text) => text,
        Err(e) => {
            log::warn!("[{session_id}] uncertainty extraction failed: {e}");
            return vec![];
        }
    };

    parse_uncertain_candidates(session_id, final_text, &raw)
}

fn build_uncertain_system_prompt() -> &'static str {
    "You extract low-confidence speech-recognition phrases for later user review.
Return strict JSON only.
Output format: array of objects with keys:
- phrase: string (original uncertain phrase)
- suggestion: string (best correction)
- reason: string (brief reason)
- confidence: number between 0 and 1
- context_text: string (short sentence context)
No markdown. No extra text."
}

fn build_uncertain_user_prompt(
    asr_text: &str,
    final_text: &str,
    interim_history: &[String],
) -> String {
    let interim_str = interim_history
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{}. {}", i + 1, t))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Find up to 8 uncertain phrases that changed across revisions or look suspicious.
Prefer technical terms, names, APIs, and command words.
If there are no meaningful uncertain phrases, return [].

ASR text:
{asr_text}

Final corrected text:
{final_text}

Interim revisions:
{interim_str}"
    )
}

fn parse_uncertain_candidates(
    session_id: &str,
    fallback_context: &str,
    raw: &str,
) -> Vec<UncertainPhraseCandidate> {
    let payload = extract_json_array_fragment(raw).unwrap_or(raw).trim();

    let parsed: Vec<UncertainPhraseCandidateRaw> = if let Ok(items) =
        serde_json::from_str::<Vec<UncertainPhraseCandidateRaw>>(payload)
    {
        items
    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
        value
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value::<UncertainPhraseCandidateRaw>(v.clone()).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    parsed
        .into_iter()
        .filter_map(|item| {
            let phrase = item.phrase.unwrap_or_default().trim().to_string();
            if phrase.is_empty() {
                return None;
            }

            let suggestion = item
                .suggestion
                .unwrap_or_else(|| phrase.clone())
                .trim()
                .to_string();
            let reason = item.reason.unwrap_or_default().trim().to_string();
            let context_text = item
                .context_text
                .unwrap_or_else(|| fallback_context.to_string())
                .trim()
                .to_string();
            let confidence = item.confidence.unwrap_or(0.5).clamp(0.0, 1.0);

            Some(UncertainPhraseCandidate {
                session_id: session_id.to_string(),
                phrase,
                suggestion,
                reason,
                confidence,
                context_text,
            })
        })
        .take(8)
        .collect()
}

fn extract_json_array_fragment(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}

async fn wait_for_final(
    asr: &mut DoubaoWsProvider,
    aggregator: &mut TranscriptAggregator,
) {
    loop {
        match asr.next_event().await {
            Ok(AsrEvent::Final(text)) => {
                aggregator.update_final(&text);
                return;
            }
            Ok(AsrEvent::Interim(text)) => {
                if !text.is_empty() {
                    aggregator.update_interim(&text);
                }
            }
            Ok(AsrEvent::Definite(text)) => {
                aggregator.update_definite(&text);
            }
            Ok(AsrEvent::Closed) => return,
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

fn cleanup_session(session_arc: &Arc<Mutex<Option<Session>>>) {
    let mut s = session_arc.lock().unwrap();
    *s = None;
}
