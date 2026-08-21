use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use std::io::Write;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crate::microphone::types::MicrophoneError;

pub struct CaptureMetrics {
    pub dropped_samples: AtomicU64,
}

pub struct CaptureStream {
    _stream: cpal::Stream,
    pub metrics: Arc<CaptureMetrics>,
}

pub fn start_capture(
    device: cpal::Device,
    mut stdin: ChildStdin,
) -> Result<(CaptureStream, u32, u16), MicrophoneError> {
    let config = device
        .default_input_config()
        .map_err(|e| MicrophoneError::StreamBuildFailed(e.to_string()))?;

    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();
    let config_stream: cpal::StreamConfig = config.into();

    // ~50ms buffer
    let ring_capacity = (sample_rate as usize * channels as usize * 50) / 1000;
    let rb = HeapRb::<f32>::new(ring_capacity);
    let (mut prod, mut cons) = rb.split();

    let metrics = Arc::new(CaptureMetrics {
        dropped_samples: AtomicU64::new(0),
    });
    let cb_metrics = metrics.clone();

    let err_fn = |err| tracing::error!("An error occurred on the capture stream: {}", err);

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config_stream.clone(),
            move |data: &[f32], _: &_| write_f32(data, &mut prod, &cb_metrics),
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config_stream.clone(),
            move |data: &[i16], _: &_| write_i16(data, &mut prod, &cb_metrics),
            err_fn,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            config_stream.clone(),
            move |data: &[i32], _: &_| write_i32(data, &mut prod, &cb_metrics),
            err_fn,
            None,
        ),
        _ => {
            return Err(MicrophoneError::UnsupportedSampleFormat(format!(
                "{:?}",
                sample_format
            )))
        }
    }
    .map_err(|e| MicrophoneError::StreamBuildFailed(e.to_string()))?;

    stream
        .play()
        .map_err(|e| MicrophoneError::StreamStartFailed(e.to_string()))?;

    // Writer thread
    thread::spawn(move || {
        let mut f32_buf = vec![0.0f32; 1024];
        loop {
            let count = cons.pop_slice(&mut f32_buf);
            if count > 0 {
                // Convert f32 slice to byte slice
                let byte_slice = unsafe {
                    std::slice::from_raw_parts(
                        f32_buf.as_ptr() as *const u8,
                        count * std::mem::size_of::<f32>(),
                    )
                };

                if let Err(e) = stdin.write_all(byte_slice) {
                    tracing::warn!("Failed to write to gstreamer stdin: {}", e);
                    break;
                }
            } else {
                // Give CPAL callback time to fill buffer
                thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    });

    Ok((
        CaptureStream {
            _stream: stream,
            metrics,
        },
        sample_rate,
        channels,
    ))
}

fn write_f32<P: Producer<Item = f32>>(
    input: &[f32],
    producer: &mut P,
    metrics: &Arc<CaptureMetrics>,
) {
    let mut dropped = 0;
    for &sample in input {
        if producer.try_push(sample).is_err() {
            dropped += 1;
        }
    }
    if dropped > 0 {
        metrics
            .dropped_samples
            .fetch_add(dropped, Ordering::Relaxed);
    }
}

fn write_i16<P: Producer<Item = f32>>(
    input: &[i16],
    producer: &mut P,
    metrics: &Arc<CaptureMetrics>,
) {
    let mut dropped = 0;
    for &sample in input {
        let val = sample as f32 / i16::MAX as f32;
        if producer.try_push(val).is_err() {
            dropped += 1;
        }
    }
    if dropped > 0 {
        metrics
            .dropped_samples
            .fetch_add(dropped, Ordering::Relaxed);
    }
}

fn write_i32<P: Producer<Item = f32>>(
    input: &[i32],
    producer: &mut P,
    metrics: &Arc<CaptureMetrics>,
) {
    let mut dropped = 0;
    for &sample in input {
        let val = sample as f32 / i32::MAX as f32;
        if producer.try_push(val).is_err() {
            dropped += 1;
        }
    }
    if dropped > 0 {
        metrics
            .dropped_samples
            .fetch_add(dropped, Ordering::Relaxed);
    }
}
