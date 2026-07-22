use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use crossbeam_channel::Sender;
use gilrs::{Gamepad, GamepadId, Gilrs};

use crate::{input::event::OwnedInputEvent, moonlight::runtime::MoonlightRuntimeHandle};

// ── Moonlight button flags from Limelight.h ──
const A_FLAG: i32 = 0x1000;
const B_FLAG: i32 = 0x2000;
const X_FLAG: i32 = 0x4000;
const Y_FLAG: i32 = 0x8000;
const UP_FLAG: i32 = 0x0001;
const DOWN_FLAG: i32 = 0x0002;
const LEFT_FLAG: i32 = 0x0004;
const RIGHT_FLAG: i32 = 0x0008;
const LB_FLAG: i32 = 0x0100;
const RB_FLAG: i32 = 0x0200;
const LS_CLK_FLAG: i32 = 0x0040;
const RS_CLK_FLAG: i32 = 0x0080;
const PLAY_FLAG: i32 = 0x0010;
const BACK_FLAG: i32 = 0x0020;
const SPECIAL_FLAG: i32 = 0x0400;

const LI_CTYPE_UNKNOWN: u8 = 0x00;
const LI_CTYPE_XBOX: u8 = 0x01;
const LI_CTYPE_PS: u8 = 0x02;
const LI_CTYPE_NINTENDO: u8 = 0x03;

const LI_CCAP_ANALOG_TRIGGERS: u16 = 0x01;
const STICK_MAX: i16 = 32767;

fn next_available_slot(active_mask: u16) -> Option<u8> {
    for i in 0..4u8 {
        if (active_mask & (1u16 << i)) == 0 {
            return Some(i);
        }
    }
    None
}

fn controller_type_from_name(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.contains("playstation") || lower.contains("dualshock") || lower.contains("dualsense") {
        LI_CTYPE_PS
    } else if lower.contains("nintendo") || lower.contains("switch") || lower.contains("snes") {
        LI_CTYPE_NINTENDO
    } else {
        LI_CTYPE_XBOX
    }
}

fn build_controller_state(gamepad: &Gamepad) -> (i32, u8, u8, i16, i16, i16, i16) {
    use gilrs::{Axis, Button};

    let mut flags: i32 = 0;

    if gamepad.is_pressed(Button::South) {
        flags |= A_FLAG;
    }
    if gamepad.is_pressed(Button::East) {
        flags |= B_FLAG;
    }
    if gamepad.is_pressed(Button::West) {
        flags |= X_FLAG;
    }
    if gamepad.is_pressed(Button::North) {
        flags |= Y_FLAG;
    }

    if gamepad.is_pressed(Button::DPadUp) {
        flags |= UP_FLAG;
    }
    if gamepad.is_pressed(Button::DPadDown) {
        flags |= DOWN_FLAG;
    }
    if gamepad.is_pressed(Button::DPadLeft) {
        flags |= LEFT_FLAG;
    }
    if gamepad.is_pressed(Button::DPadRight) {
        flags |= RIGHT_FLAG;
    }

    if gamepad.is_pressed(Button::LeftTrigger) || gamepad.is_pressed(Button::LeftTrigger2) {
        flags |= LB_FLAG;
    }
    if gamepad.is_pressed(Button::RightTrigger) || gamepad.is_pressed(Button::RightTrigger2) {
        flags |= RB_FLAG;
    }

    if gamepad.is_pressed(Button::LeftThumb) {
        flags |= LS_CLK_FLAG;
    }
    if gamepad.is_pressed(Button::RightThumb) {
        flags |= RS_CLK_FLAG;
    }
    if gamepad.is_pressed(Button::Start) {
        flags |= PLAY_FLAG;
    }
    if gamepad.is_pressed(Button::Select) {
        flags |= BACK_FLAG;
    }
    if gamepad.is_pressed(Button::Mode) {
        flags |= SPECIAL_FLAG;
    }

    let left_trigger = (gamepad.value(Axis::LeftZ) * 255.0).clamp(0.0, 255.0) as u8;
    let right_trigger = (gamepad.value(Axis::RightZ) * 255.0).clamp(0.0, 255.0) as u8;

    let to_i16 = |v: f32| (v * STICK_MAX as f32).clamp(-32767.0, 32767.0) as i16;
    let left_stick_x = to_i16(gamepad.value(Axis::LeftStickX));
    let left_stick_y = to_i16(gamepad.value(Axis::LeftStickY));
    let right_stick_x = to_i16(gamepad.value(Axis::RightStickX));
    let right_stick_y = to_i16(gamepad.value(Axis::RightStickY));

    (
        flags,
        left_trigger,
        right_trigger,
        left_stick_x,
        left_stick_y,
        right_stick_x,
        right_stick_y,
    )
}

pub struct ControllerManager {
    running: Arc<AtomicBool>,
}

impl ControllerManager {
    pub fn start(runtime: MoonlightRuntimeHandle, _events_tx: Sender<OwnedInputEvent>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);

        thread::Builder::new()
            .name("moonlight-controller".into())
            .spawn(move || run_controller_poll(thread_running, runtime))
            .expect("failed to start controller poll thread");

        Self { running }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

fn run_controller_poll(running: Arc<AtomicBool>, runtime: MoonlightRuntimeHandle) {
    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(error) => {
            tracing::warn!(?error, "failed to initialise gilrs gamepad library");
            return;
        }
    };

    let mut assignments: HashMap<GamepadId, u8> = HashMap::new();
    let mut active_mask: u16 = 0;
    let mut known_ids: Vec<GamepadId> = Vec::new();

    while running.load(Ordering::Acquire) {
        let current_ids: Vec<GamepadId> = gilrs.gamepads().map(|(id, _)| id).collect();

        for &id in &current_ids {
            if !known_ids.contains(&id) {
                if let Some(index) = next_available_slot(active_mask) {
                    let gamepad = gilrs.gamepad(id);
                    let ctype = controller_type_from_name(gamepad.name());
                    active_mask |= 1u16 << index;
                    assignments.insert(id, index);
                    send_arrival(&runtime, index, active_mask, ctype);
                }
            }
        }

        known_ids.retain(|id| {
            if current_ids.contains(id) {
                return true;
            }
            if let Some(index) = assignments.remove(id) {
                active_mask &= !(1u16 << index);
                send_removal(&runtime, index, active_mask);
            }
            false
        });
        for &id in &current_ids {
            if !known_ids.contains(&id) {
                known_ids.push(id);
            }
        }

        for (id, &index) in &assignments {
            if let Some(gamepad) = gilrs.connected_gamepad(*id) {
                let (flags, lt, rt, lsx, lsy, rsx, rsy) = build_controller_state(&gamepad);
                send_state(
                    &runtime,
                    index as i16,
                    active_mask,
                    flags,
                    lt,
                    rt,
                    lsx,
                    lsy,
                    rsx,
                    rsy,
                );
            }
        }

        thread::sleep(Duration::from_millis(4));
    }

    for &index in assignments.values() {
        active_mask &= !(1u16 << index);
        send_removal(&runtime, index, active_mask);
    }
}

fn send_arrival(runtime: &MoonlightRuntimeHandle, index: u8, active_mask: u16, ctype: u8) {
    tracing::info!(index, active_mask, ctype, "controller arrival");
    let _ = tauri::async_runtime::block_on(runtime.send_controller_arrival(
        index,
        active_mask,
        ctype,
        0,
        LI_CCAP_ANALOG_TRIGGERS,
    ));
}

fn send_removal(runtime: &MoonlightRuntimeHandle, index: u8, active_mask: u16) {
    tracing::info!(index, active_mask, "controller removal");
    let _ = tauri::async_runtime::block_on(runtime.send_controller_arrival(
        index,
        active_mask,
        LI_CTYPE_UNKNOWN,
        0,
        0,
    ));
}

fn send_state(
    runtime: &MoonlightRuntimeHandle,
    controller_number: i16,
    active_mask: u16,
    button_flags: i32,
    left_trigger: u8,
    right_trigger: u8,
    left_stick_x: i16,
    left_stick_y: i16,
    right_stick_x: i16,
    right_stick_y: i16,
) {
    let _ = tauri::async_runtime::block_on(runtime.send_controller_state(
        controller_number,
        active_mask as i16,
        button_flags,
        left_trigger,
        right_trigger,
        left_stick_x,
        left_stick_y,
        right_stick_x,
        right_stick_y,
    ));
}
