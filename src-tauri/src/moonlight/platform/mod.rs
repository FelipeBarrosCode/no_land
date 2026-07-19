pub mod window;

pub use window::{
    close_stream_window, create_or_reuse_stream_window, stream_window_surface_descriptor,
    NativeSurfaceDescriptor, STREAM_WINDOW_LABEL,
};
