pub mod macos_input;
pub mod window;

pub use macos_input::{
    activate_native_stream_input, deactivate_native_stream_input, install_native_stream_input,
    set_native_stream_input_debug_overlay_enabled, uninstall_native_stream_input,
};
pub use window::{
    close_stream_window, create_or_reuse_stream_window, stream_window_surface_descriptor,
    NativeSurfaceDescriptor, StreamWindowCloseState, STREAM_WINDOW_LABEL,
};
