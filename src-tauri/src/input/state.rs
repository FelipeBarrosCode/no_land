#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    Relative,
    Absolute,
}

#[derive(Debug, Clone)]
pub struct CaptureState {
    pub active: bool,
    pub focused: bool,
    pub mouse_mode: MouseMode,
    pub video_width: u32,
    pub video_height: u32,
    pub video_left: f64,
    pub video_top: f64,
    pub video_render_width: f64,
    pub video_render_height: f64,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            active: false,
            focused: false,
            mouse_mode: MouseMode::Relative,
            video_width: 1920,
            video_height: 1080,
            video_left: 0.0,
            video_top: 0.0,
            video_render_width: 1920.0,
            video_render_height: 1080.0,
        }
    }
}
