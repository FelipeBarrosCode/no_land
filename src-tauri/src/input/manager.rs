use std::{collections::HashSet, sync::Arc};

use parking_lot::Mutex;

use crate::{
    input::{
        event::{ButtonState, InputEvent, MouseButton, OwnedInputEvent},
        mapping::{map_to_video, VideoRect},
        state::{CaptureState, MouseMode},
        worker::{start_input_worker, InputWorkerHandle},
    },
    moonlight::runtime::MoonlightRuntimeHandle,
};

#[derive(Default)]
struct PressedInputState {
    mouse_buttons: HashSet<MouseButton>,
    keys: HashSet<u16>,
}

pub struct InputManager {
    state: Mutex<CaptureState>,
    pressed: Mutex<PressedInputState>,
    worker: InputWorkerHandle,
}

impl InputManager {
    pub fn new(runtime: MoonlightRuntimeHandle) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(CaptureState::default()),
            pressed: Mutex::new(PressedInputState::default()),
            worker: start_input_worker(runtime),
        })
    }

    pub fn begin_capture(&self, mode: MouseMode) {
        let mut state = self.state.lock();
        state.active = true;
        state.focused = true;
        state.mouse_mode = mode;
    }

    pub fn end_capture(&self) {
        {
            let mut state = self.state.lock();
            state.active = false;
            state.focused = false;
        }
        self.release_all_inputs();
    }

    pub fn set_focus(&self, focused: bool) {
        {
            let mut state = self.state.lock();
            state.focused = focused;
        }
        if !focused {
            self.release_all_inputs();
        }
    }

    pub fn capture_state(&self) -> CaptureState {
        self.state.lock().clone()
    }

    pub fn relative_motion(&self, dx: i32, dy: i32) {
        let state = self.state.lock();
        if !state.active || !state.focused || state.mouse_mode != MouseMode::Relative {
            return;
        }
        drop(state);
        self.worker.motion.add(dx, dy);
    }

    pub fn absolute_motion(&self, pointer_x: f64, pointer_y: f64) {
        let state = self.state.lock();
        if !state.active || !state.focused || state.mouse_mode != MouseMode::Absolute {
            return;
        }
        let rect = VideoRect {
            left: state.video_left,
            top: state.video_top,
            width: state.video_render_width,
            height: state.video_render_height,
        };
        drop(state);

        let Some(position) = map_to_video(pointer_x, pointer_y, rect) else {
            return;
        };

        self.send_ordered(InputEvent::AbsoluteMouseMove {
            x: position.x as i32,
            y: position.y as i32,
            reference_width: position.reference_width as i32,
            reference_height: position.reference_height as i32,
        });
    }

    pub fn mouse_button(&self, button: MouseButton, state: ButtonState) {
        {
            let mut pressed = self.pressed.lock();
            match state {
                ButtonState::Pressed => {
                    pressed.mouse_buttons.insert(button);
                }
                ButtonState::Released => {
                    pressed.mouse_buttons.remove(&button);
                }
            }
        }

        self.send_ordered(InputEvent::MouseButton { button, state });
    }

    pub fn vertical_scroll(&self, amount: i32, high_resolution: bool) {
        self.send_ordered(InputEvent::VerticalScroll {
            amount,
            high_resolution,
        });
    }

    pub fn horizontal_scroll(&self, amount: i32, high_resolution: bool) {
        self.send_ordered(InputEvent::HorizontalScroll {
            amount,
            high_resolution,
        });
    }

    pub fn key(&self, virtual_key: u16, state: ButtonState, modifiers: u8, non_normalized: bool) {
        {
            let mut pressed = self.pressed.lock();
            match state {
                ButtonState::Pressed => {
                    pressed.keys.insert(virtual_key);
                }
                ButtonState::Released => {
                    pressed.keys.remove(&virtual_key);
                }
            }
        }

        self.send_ordered(InputEvent::Key {
            virtual_key,
            state,
            modifiers,
            non_normalized,
        });
    }

    pub fn update_video_geometry(&self, left: f64, top: f64, width: f64, height: f64) {
        let mut state = self.state.lock();
        state.video_left = left;
        state.video_top = top;
        state.video_render_width = width;
        state.video_render_height = height;
        state.video_width = width.round().clamp(0.0, u32::MAX as f64) as u32;
        state.video_height = height.round().clamp(0.0, u32::MAX as f64) as u32;
    }

    pub fn release_all_inputs(&self) {
        let (buttons, keys) = {
            let mut pressed = self.pressed.lock();
            (
                pressed.mouse_buttons.drain().collect::<Vec<_>>(),
                pressed.keys.drain().collect::<Vec<_>>(),
            )
        };

        for button in buttons {
            let _ =
                self.worker
                    .events
                    .try_send(OwnedInputEvent::Immediate(InputEvent::MouseButton {
                        button,
                        state: ButtonState::Released,
                    }));
        }

        for virtual_key in keys {
            let _ = self
                .worker
                .events
                .try_send(OwnedInputEvent::Immediate(InputEvent::Key {
                    virtual_key,
                    state: ButtonState::Released,
                    modifiers: 0,
                    non_normalized: false,
                }));
        }

        let _ = self.worker.events.try_send(OwnedInputEvent::ReleaseAll);
    }

    pub fn stop_worker(&self) {
        self.worker.stop();
    }

    fn send_ordered(&self, event: InputEvent) {
        let state = self.state.lock();
        if !state.active || !state.focused {
            return;
        }
        drop(state);

        if let Err(error) = self
            .worker
            .events
            .try_send(OwnedInputEvent::Immediate(event))
        {
            tracing::warn!(?error, "input queue full");
        }
    }
}
