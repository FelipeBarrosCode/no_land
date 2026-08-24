import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

old_status = """                        match handle.status() {
                            Ok(local_status) => {
                                status.muted = local_status
                                    .get("muted")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false);
                                status.capture_sample_rate = local_status
                                    .get("activeSampleRate")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0)
                                    as u32;
                                status.sidecar_healthy = local_status
                                    .get("health")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|health| health != "failed");
                                if let Some(error) = local_status
                                    .get("lastError")
                                    .and_then(serde_json::Value::as_str)
                                {
                                    status.error = Some(error.to_string());
                                }
                            }
                            Err(error) => status.error = Some(error.to_string()),
                        }
                        if let Ok(metrics) = handle.metrics() {
                            status.capture_overruns = metrics
                                .get("overruns")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            let ring_samples = metrics
                                .get("ringDepthSamples")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            status.ring_fill_ms = if status.capture_sample_rate > 0 {
                                ring_samples as f64 * 1_000.0 / status.capture_sample_rate as f64
                            } else {
                                0.0
                            };
                            status.appsrc_queue_ms = metrics
                                .get("appsrcQueueMs")
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or(0.0);
                            status.opus_packets_sent = metrics
                                .get("opusPacketsSent")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                            status.bytes_sent = metrics
                                .get("bytesSent")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0);
                        }"""

new_status = """                        status.sidecar_healthy = true;
                        status.muted = false;
                        status.capture_sample_rate = 48000;
                        let dropped = handle.stream.metrics.dropped_samples.load(std::sync::atomic::Ordering::Relaxed);
                        status.capture_overruns = dropped;"""

code = code.replace(old_status, new_status)

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)

