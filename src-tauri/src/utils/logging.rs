use tracing_subscriber::{fmt, EnvFilter};

pub fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("noland_connect=info,tauri=info,reqwest=warn,hyper=warn")
    });

    let _ = fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(false)
        .compact()
        .try_init();
}
