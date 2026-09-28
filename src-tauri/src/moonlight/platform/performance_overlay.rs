use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

use crate::moonlight::{domain::MoonlightConfiguration, runtime::RuntimeStatistics};

pub fn preference(
    configuration: &MoonlightConfiguration,
    host_id: &str,
    global_default: bool,
) -> bool {
    configuration
        .hosts
        .get(host_id)
        .and_then(|host| host.preferences_override.as_ref())
        .and_then(|prefs| prefs.window.as_ref())
        .and_then(|window| window.show_statistics)
        .or_else(|| configuration.pending_statistics.get(host_id).copied())
        .unwrap_or(global_default)
}

fn number(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "—".into())
}

pub fn set_preference(configuration: &mut MoonlightConfiguration, host_id: &str, enabled: bool) {
    if let Some(host) = configuration.hosts.get_mut(host_id) {
        let overrides = host
            .preferences_override
            .get_or_insert_with(Default::default);
        let window = overrides.window.get_or_insert_with(Default::default);
        window.show_statistics = Some(enabled);
        configuration.pending_statistics.remove(host_id);
    } else {
        configuration
            .pending_statistics
            .insert(host_id.to_owned(), enabled);
    }
}

fn text(stats: &RuntimeStatistics, probe: Option<&serde_json::Value>) -> String {
    let p = &stats.performance;
    let mut result = format!(
        "NO LAND · STREAM STATISTICS\n{} × {} · {} · target {} FPS\nFPS incoming {:.1} · decoded {} · submitted {:.1}\nVideo {:.2} Mbps (encoded payload)\nStream RTT {} ms · RTT variation {} ms\nHost {} ms · reassembly {} ms\nDecode {} ms · render queue {} ms\nPredecode gaps {:.2}% · local drops {} total\nFEC recovered {} packets · failed {} blocks / tick\nQueues core {} · decode {} · render {}\nPacing {} · packet {} B",
        p.width, p.height, p.codec, stats.stream_fps,
        p.incoming_fps, number(p.decoded_fps), p.submitted_fps, p.video_mbps,
        number(stats.estimated_rtt_ms.map(f64::from)), number(stats.estimated_rtt_variance_ms.map(f64::from)),
        number(p.host_processing_ms), number(p.reassembly_ms), number(p.decode_ms), number(p.render_queue_ms),
        p.missing_frames_percent,
        stats.adaptive_stale_drop_count + stats.pacer_backlog_drop_count + stats.renderer_error_drop_count + stats.smoothing_overflow_drops,
        stats.fec_recoveries_interval, stats.fec_failures_interval,
        stats.pending_core_video_frames, stats.decoder_queue_depth, stats.render_queue_depth,
        stats.effective_pacing_mode, stats.requested_packet_size,
    );
    if let Some(probe) = probe {
        let current = &probe["metrics"]["current"];
        let window = &probe["metrics"]["last60Seconds"];
        result.push_str(&format!(
            "\nProbe RTT {} ms · jitter {} ms\nProbe loss {}% · P95 {} ms (60 s)",
            number(
                current["rttMs"]
                    .as_f64()
                    .filter(|_| current["lost"].as_bool() == Some(false))
            ),
            number(
                current["jitterMs"]
                    .as_f64()
                    .filter(|_| window["received"].as_u64().unwrap_or(0) >= 2)
            ),
            number(window["lossPercent"].as_f64()),
            number(window["p95RttMs"].as_f64())
        ));
    } else {
        result.push_str("\nProbe jitter / loss: unavailable or warming up");
    }
    result.push_str(if p.samples == 0 {
        "\nWaiting for video samples…"
    } else {
        "\n1 s video window · submitted FPS ≠ scanout"
    });
    result
}

/// A single bounded updater for the native window, which has no React webview.
pub fn start(app: AppHandle, enabled: Arc<Mutex<Option<(String, bool)>>>) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let Some(window) = app.get_window(super::STREAM_WINDOW_LABEL) else {
                clear_composited_text(&app);
                continue;
            };
            let visible = enabled
                .lock()
                .ok()
                .is_some_and(|state| state.as_ref().is_some_and(|(_, enabled)| *enabled));
            let stats = app
                .state::<crate::moonlight::composition::MoonlightManager>()
                .runtime
                .latest_statistics();
            let probe = app
                .state::<crate::network_monitor::NetworkMonitor>()
                .fresh_snapshot()
                .await;
            let content = if visible && stats.state == "streaming" {
                if stats.sampled_at.elapsed() > std::time::Duration::from_secs(2) {
                    "NO LAND · Waiting for stream statistics…".into()
                } else {
                    text(&stats, probe.as_ref())
                }
            } else {
                String::new()
            };

            // Linux composites the text into the video pipeline, which is the
            // only reliable stacking above X11/Wayland video sinks.
            #[cfg(target_os = "linux")]
            {
                let _ = app
                    .state::<crate::moonlight::composition::MoonlightManager>()
                    .runtime
                    .set_overlay_text(content);
                continue;
            }

            // macOS and Windows own non-interactive child surfaces that sit
            // above the video. Wait for each main-thread update before the next.
            #[cfg(not(target_os = "linux"))]
            {
                let (done, wait) = tokio::sync::oneshot::channel();
                if app
                    .run_on_main_thread(move || {
                        update(&window, &content);
                        let _ = done.send(());
                    })
                    .is_err()
                {
                    break;
                }
                if wait.await.is_err() {
                    break;
                }
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn clear_composited_text(app: &AppHandle) {
    let _ = app
        .state::<crate::moonlight::composition::MoonlightManager>()
        .runtime
        .set_overlay_text(String::new());
}

#[cfg(not(target_os = "linux"))]
fn clear_composited_text(_app: &AppHandle) {}

#[cfg(not(target_os = "linux"))]
fn update(window: &tauri::Window, text: &str) {
    #[cfg(not(test))]
    {
        unsafe extern "C" {
            fn noland_performance_overlay_update(
                handle: *mut std::ffi::c_void,
                text: *const std::ffi::c_char,
            );
        }
        let Ok(surface) = super::stream_window_surface_descriptor(window) else {
            return;
        };
        let Ok(text) = std::ffi::CString::new(text) else {
            return;
        };
        unsafe {
            noland_performance_overlay_update(surface.window_handle as *mut _, text.as_ptr())
        };
    }
    #[cfg(test)]
    let _ = (window, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_and_host_choices_override_global_default() {
        let mut config = MoonlightConfiguration::default();
        assert!(preference(&config, "instance-1", true));
        config.pending_statistics.insert("instance-1".into(), false);
        assert!(!preference(&config, "instance-1", true));
        assert!(preference(&config, "instance-2", true));
    }

    #[test]
    fn unavailable_values_are_not_reported_as_zero() {
        assert_eq!(number(None), "—");
        assert_eq!(number(Some(-1.0)), "—");
        assert_eq!(number(Some(f64::NAN)), "—");
        assert_eq!(number(Some(0.0)), "0.0");
    }
}
