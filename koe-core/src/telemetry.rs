use crate::ffi::invoke_log_event;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::env;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

/// Metrics collected for each session.
pub struct SessionMetrics {
    pub session_id: String,
    pub hotkey_start: Option<Instant>,
    pub hotkey_end: Option<Instant>,
    pub asr_connect_start: Option<Instant>,
    pub asr_connected: Option<Instant>,
    pub asr_final_received: Option<Instant>,
    pub llm_start: Option<Instant>,
    pub llm_end: Option<Instant>,
    pub paste_done: Option<Instant>,
    pub clipboard_restored: Option<Instant>,
    pub error_type: Option<String>,
    pub auto_pasted: bool,
}

impl SessionMetrics {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            hotkey_start: None,
            hotkey_end: None,
            asr_connect_start: None,
            asr_connected: None,
            asr_final_received: None,
            llm_start: None,
            llm_end: None,
            paste_done: None,
            clipboard_restored: None,
            error_type: None,
            auto_pasted: false,
        }
    }

    fn duration_ms(start: Option<Instant>, end: Option<Instant>) -> Option<u64> {
        match (start, end) {
            (Some(s), Some(e)) => Some(e.duration_since(s).as_millis() as u64),
            _ => None,
        }
    }

    pub fn recording_duration_ms(&self) -> Option<u64> {
        Self::duration_ms(self.hotkey_start, self.hotkey_end)
    }

    pub fn asr_connect_duration_ms(&self) -> Option<u64> {
        Self::duration_ms(self.asr_connect_start, self.asr_connected)
    }

    pub fn asr_finalize_duration_ms(&self) -> Option<u64> {
        Self::duration_ms(self.hotkey_end, self.asr_final_received)
    }

    pub fn llm_duration_ms(&self) -> Option<u64> {
        Self::duration_ms(self.llm_start, self.llm_end)
    }

    pub fn summary(&self) -> String {
        format!(
            "session={} recording={}ms asr_connect={}ms asr_finalize={}ms llm={}ms pasted={} error={:?}",
            self.session_id,
            self.recording_duration_ms().map_or("?".into(), |v| v.to_string()),
            self.asr_connect_duration_ms().map_or("?".into(), |v| v.to_string()),
            self.asr_finalize_duration_ms().map_or("?".into(), |v| v.to_string()),
            self.llm_duration_ms().map_or("?".into(), |v| v.to_string()),
            self.auto_pasted,
            self.error_type,
        )
    }
}

struct KoeLogger {
    level: LevelFilter,
    file: Option<Mutex<File>>,
}

impl KoeLogger {
    fn new(level: LevelFilter) -> Self {
        Self {
            level,
            file: open_log_file().map(Mutex::new),
        }
    }
}

impl Log for KoeLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let line = format!("[{ts_ms}] [{}] {}\n", record.level(), record.args());

        eprint!("{line}");

        if let Some(file) = &self.file {
            if let Ok(mut handle) = file.lock() {
                let _ = handle.write_all(line.as_bytes());
                let _ = handle.flush();
            }
        }

        invoke_log_event(level_to_ffi(record.level()), line.trim_end());
    }

    fn flush(&self) {
        if let Some(file) = &self.file {
            if let Ok(mut handle) = file.lock() {
                let _ = handle.flush();
            }
        }
    }
}

fn level_to_ffi(level: Level) -> i32 {
    match level {
        Level::Error => 0,
        Level::Warn => 1,
        Level::Info => 2,
        Level::Debug | Level::Trace => 3,
    }
}

fn open_log_file() -> Option<File> {
    let home = env::var_os("HOME")?;
    let mut dir = PathBuf::from(home);
    dir.push(".koe");
    dir.push("logs");

    if let Err(e) = create_dir_all(&dir) {
        eprintln!("[Koe] failed to create log directory {}: {e}", dir.display());
        return None;
    }

    let mut file_path = dir;
    file_path.push("koe.log");

    match OpenOptions::new().create(true).append(true).open(&file_path) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("[Koe] failed to open log file {}: {e}", file_path.display());
            None
        }
    }
}

pub fn init_logging() {
    static INIT: Once = Once::new();
    static LOGGER: OnceLock<KoeLogger> = OnceLock::new();

    INIT.call_once(|| {
        let _ = LOGGER.set(KoeLogger::new(LevelFilter::Info));
        if let Some(logger) = LOGGER.get() {
            let _ = log::set_logger(logger);
            log::set_max_level(LevelFilter::Info);
        }
    });
}
