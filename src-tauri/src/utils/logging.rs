use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use tracing_subscriber::{fmt, fmt::MakeWriter, EnvFilter};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Clone, Copy)]
struct NolandLogWriter;

struct NolandLogSink;

impl Write for NolandLogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        if let Some(file) = LOG_FILE.get() {
            if let Ok(mut guard) = file.lock() {
                if let Some(file) = guard.as_mut() {
                    let _ = file.write_all(buf);
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Some(file) = LOG_FILE.get() {
            if let Ok(mut guard) = file.lock() {
                if let Some(file) = guard.as_mut() {
                    let _ = file.flush();
                }
            }
        }
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for NolandLogWriter {
    type Writer = NolandLogSink;

    fn make_writer(&'writer self) -> Self::Writer {
        NolandLogSink
    }
}

fn default_log_path() -> Option<PathBuf> {
    let base = dirs::data_local_dir().or_else(dirs::data_dir)?;
    Some(
        base.join("com.noland.connect")
            .join("logs")
            .join("noland-connect.log"),
    )
}

pub fn log_file_path() -> Option<PathBuf> {
    LOG_PATH.get().cloned().or_else(default_log_path)
}

pub fn recent_log_excerpt(max_lines: usize) -> io::Result<String> {
    let Some(path) = log_file_path() else {
        return Ok("Log path unavailable".to_string());
    };
    let mut content = String::new();
    File::open(path)?.read_to_string(&mut content)?;
    let mut lines = content.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.join("\n"))
}

pub fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("noland_connect=info,tauri=info,reqwest=warn,hyper=warn")
    });

    if let Some(path) = default_log_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        let _ = LOG_PATH.set(path);
        let _ = LOG_FILE.set(Mutex::new(file));
    }

    let _ = fmt()
        .with_env_filter(env_filter)
        .with_writer(NolandLogWriter)
        .with_target(true)
        .with_thread_ids(false)
        .compact()
        .try_init();
}
