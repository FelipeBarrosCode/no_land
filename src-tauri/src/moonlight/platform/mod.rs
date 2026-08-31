pub mod desktop_input;
pub mod window;

pub use desktop_input::{
    activate_native_stream_input, deactivate_native_stream_input, install_native_stream_input,
    set_native_stream_input_debug_overlay_enabled,
};
pub use window::{
    close_stream_window, create_or_reuse_stream_window, stream_window_surface_descriptor,
    NativeSurfaceDescriptor, StreamWindowCloseState, STREAM_WINDOW_LABEL,
};
