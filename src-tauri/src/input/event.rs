#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    X1,
    X2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy)]
pub enum InputEvent {
    RelativeMouseMove {
        dx: i32,
        dy: i32,
    },
    AbsoluteMouseMove {
        x: i32,
        y: i32,
        reference_width: i32,
        reference_height: i32,
    },
    MouseButton {
        button: MouseButton,
        state: ButtonState,
    },
    VerticalScroll {
        amount: i32,
        high_resolution: bool,
    },
    HorizontalScroll {
        amount: i32,
        high_resolution: bool,
    },
    Key {
        virtual_key: u16,
        state: ButtonState,
        modifiers: u8,
        non_normalized: bool,
    },
}

#[derive(Debug, Clone)]
pub enum OwnedInputEvent {
    Immediate(InputEvent),
    ReleaseAll,
}
