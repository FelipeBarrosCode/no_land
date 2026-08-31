use std::{
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use crossbeam_channel::{bounded, Receiver, Sender};

use crate::{
    input::event::{ButtonState, InputEvent, MouseButton, OwnedInputEvent},
    moonlight::runtime::MoonlightRuntimeHandle,
};

static RELATIVE_SEND_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static ABSOLUTE_SEND_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static BUTTON_SEND_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static KEY_SEND_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static SCROLL_SEND_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static SEND_ERRORS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputWorkerDebugSnapshot {
    pub relative_send_attempts: u64,
    pub absolute_send_attempts: u64,
    pub button_send_attempts: u64,
    pub key_send_attempts: u64,
    pub scroll_send_attempts: u64,
    pub send_errors: u64,
}

pub fn input_worker_debug_snapshot() -> InputWorkerDebugSnapshot {
    InputWorkerDebugSnapshot {
        relative_send_attempts: RELATIVE_SEND_ATTEMPTS.load(Ordering::Relaxed),
        absolute_send_attempts: ABSOLUTE_SEND_ATTEMPTS.load(Ordering::Relaxed),
        button_send_attempts: BUTTON_SEND_ATTEMPTS.load(Ordering::Relaxed),
        key_send_attempts: KEY_SEND_ATTEMPTS.load(Ordering::Relaxed),
        scroll_send_attempts: SCROLL_SEND_ATTEMPTS.load(Ordering::Relaxed),
        send_errors: SEND_ERRORS.load(Ordering::Relaxed),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_relative_send_attempts() -> u64 {
    RELATIVE_SEND_ATTEMPTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_absolute_send_attempts() -> u64 {
    ABSOLUTE_SEND_ATTEMPTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_button_send_attempts() -> u64 {
    BUTTON_SEND_ATTEMPTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_key_send_attempts() -> u64 {
    KEY_SEND_ATTEMPTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_scroll_send_attempts() -> u64 {
    SCROLL_SEND_ATTEMPTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn noland_input_debug_send_errors() -> u64 {
    SEND_ERRORS.load(Ordering::Relaxed)
}

pub struct MotionAccumulator {
    dx: AtomicI32,
    dy: AtomicI32,
}

impl MotionAccumulator {
    pub fn new() -> Self {
        Self {
            dx: AtomicI32::new(0),
            dy: AtomicI32::new(0),
        }
    }

    pub fn add(&self, dx: i32, dy: i32) {
        self.dx.fetch_add(dx, Ordering::Relaxed);
        self.dy.fetch_add(dy, Ordering::Relaxed);
    }

    pub fn take(&self) -> (i32, i32) {
        (
            self.dx.swap(0, Ordering::AcqRel),
            self.dy.swap(0, Ordering::AcqRel),
        )
    }
}

pub struct InputWorkerHandle {
    pub events: Sender<OwnedInputEvent>,
    pub motion: Arc<MotionAccumulator>,
}

pub fn start_input_worker(runtime: MoonlightRuntimeHandle) -> InputWorkerHandle {
    let (tx, rx) = bounded(2048);
    let motion = Arc::new(MotionAccumulator::new());
    let running = Arc::new(AtomicBool::new(true));

    let thread_motion = Arc::clone(&motion);
    let thread_running = Arc::clone(&running);

    thread::Builder::new()
        .name("moonlight-input".into())
        .spawn(move || run_input_worker(rx, thread_motion, thread_running, runtime))
        .expect("failed to start input worker");

    InputWorkerHandle { events: tx, motion }
}

fn run_input_worker(
    rx: Receiver<OwnedInputEvent>,
    motion: Arc<MotionAccumulator>,
    running: Arc<AtomicBool>,
    runtime: MoonlightRuntimeHandle,
) {
    while running.load(Ordering::Acquire) {
        flush_motion(&motion, &runtime);

        while let Ok(event) = rx.try_recv() {
            flush_motion(&motion, &runtime);
            handle_ordered_event(event, &runtime);
        }

        thread::sleep(Duration::from_micros(500));
    }

    flush_motion(&motion, &runtime);
}

fn flush_motion(motion: &MotionAccumulator, runtime: &MoonlightRuntimeHandle) {
    let (mut dx, mut dy) = motion.take();

    while dx != 0 || dy != 0 {
        let packet_dx = dx.clamp(i16::MIN as i32, i16::MAX as i32);
        let packet_dy = dy.clamp(i16::MIN as i32, i16::MAX as i32);
        RELATIVE_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        if tauri::async_runtime::block_on(
            runtime.send_relative_mouse(packet_dx as i16, packet_dy as i16),
        )
        .is_err()
        {
            SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
        dx -= packet_dx;
        dy -= packet_dy;
    }
}

fn handle_ordered_event(event: OwnedInputEvent, runtime: &MoonlightRuntimeHandle) {
    match event {
        OwnedInputEvent::Immediate(InputEvent::AbsoluteMouseMove {
            x,
            y,
            reference_width,
            reference_height,
        }) => {
            ABSOLUTE_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if tauri::async_runtime::block_on(runtime.send_absolute_mouse(
                x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                reference_width.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                reference_height.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            ))
            .is_err()
            {
                SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
        OwnedInputEvent::Immediate(InputEvent::MouseButton { button, state }) => {
            BUTTON_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if tauri::async_runtime::block_on(runtime.send_mouse_button(
                map_mouse_button(button),
                matches!(state, ButtonState::Pressed),
            ))
            .is_err()
            {
                SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
        OwnedInputEvent::Immediate(InputEvent::VerticalScroll {
            amount,
            high_resolution,
        }) => {
            SCROLL_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if tauri::async_runtime::block_on(runtime.send_vertical_scroll(
                amount.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                high_resolution,
            ))
            .is_err()
            {
                SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
        OwnedInputEvent::Immediate(InputEvent::HorizontalScroll {
            amount,
            high_resolution,
        }) => {
            SCROLL_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if tauri::async_runtime::block_on(runtime.send_horizontal_scroll(
                amount.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                high_resolution,
            ))
            .is_err()
            {
                SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }
        OwnedInputEvent::Immediate(InputEvent::Key {
            virtual_key,
            state,
            modifiers,
            ..
        }) => {
            KEY_SEND_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            if tauri::async_runtime::block_on(runtime.send_keyboard(
                virtual_key,
                matches!(state, ButtonState::Pressed),
                modifiers,
            ))
            .is_err()
            {
                SEND_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
        }

        OwnedInputEvent::ReleaseAll => {}
    }
}

fn map_mouse_button(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0x01,
        MouseButton::Middle => 0x02,
        MouseButton::Right => 0x03,
        MouseButton::X1 => 0x04,
        MouseButton::X2 => 0x05,
    }
}

#[cfg(test)]
mod tests {
    use super::MotionAccumulator;

    #[test]
    fn accumulates_relative_motion() {
        let accumulator = MotionAccumulator::new();
        accumulator.add(10, 4);
        accumulator.add(-3, 8);
        assert_eq!(accumulator.take(), (7, 12));
        assert_eq!(accumulator.take(), (0, 0));
    }
}
