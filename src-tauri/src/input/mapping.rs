#[derive(Debug, Clone, Copy)]
pub struct VideoRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct AbsolutePosition {
    pub x: i16,
    pub y: i16,
    pub reference_width: i16,
    pub reference_height: i16,
}

pub fn aspect_fit_video_rect(
    content_width: f64,
    content_height: f64,
    video_width: u32,
    video_height: u32,
) -> Option<VideoRect> {
    if content_width <= 0.0 || content_height <= 0.0 || video_width == 0 || video_height == 0 {
        return None;
    }

    let source_aspect = video_width as f64 / video_height as f64;
    let content_aspect = content_width / content_height;

    if content_aspect > source_aspect {
        let height = content_height;
        let width = height * source_aspect;
        Some(VideoRect {
            left: (content_width - width) / 2.0,
            top: 0.0,
            width,
            height,
        })
    } else {
        let width = content_width;
        let height = width / source_aspect;
        Some(VideoRect {
            left: 0.0,
            top: (content_height - height) / 2.0,
            width,
            height,
        })
    }
}

pub fn map_to_video(pointer_x: f64, pointer_y: f64, rect: VideoRect) -> Option<AbsolutePosition> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }

    let local_x = pointer_x - rect.left;
    let local_y = pointer_y - rect.top;

    if local_x < 0.0 || local_y < 0.0 || local_x > rect.width || local_y > rect.height {
        return None;
    }

    let width = rect.width.round().clamp(1.0, i16::MAX as f64) as i16;
    let height = rect.height.round().clamp(1.0, i16::MAX as f64) as i16;
    let x = local_x.round().clamp(0.0, width as f64) as i16;
    let y = local_y.round().clamp(0.0, height as f64) as i16;

    Some(AbsolutePosition {
        x,
        y,
        reference_width: width,
        reference_height: height,
    })
}

#[cfg(test)]
mod tests {
    use super::{aspect_fit_video_rect, map_to_video, VideoRect};

    #[test]
    fn maps_center_of_video() {
        let result = map_to_video(
            800.0,
            500.0,
            VideoRect {
                left: 0.0,
                top: 50.0,
                width: 1600.0,
                height: 900.0,
            },
        )
        .unwrap();

        assert_eq!(result.x, 800);
        assert_eq!(result.y, 450);
        assert_eq!(result.reference_width, 1600);
        assert_eq!(result.reference_height, 900);
    }

    #[test]
    fn rejects_letterbox_area() {
        assert!(map_to_video(
            800.0,
            20.0,
            VideoRect {
                left: 0.0,
                top: 50.0,
                width: 1600.0,
                height: 900.0,
            },
        )
        .is_none());
    }

    #[test]
    fn computes_aspect_fit_video_rect_for_letterboxed_view() {
        let rect = aspect_fit_video_rect(1600.0, 1000.0, 1920, 1080).unwrap();
        assert_eq!(rect.left, 0.0);
        assert_eq!(rect.top, 50.0);
        assert_eq!(rect.width, 1600.0);
        assert_eq!(rect.height, 900.0);
    }

    #[test]
    fn computes_aspect_fit_video_rect_for_pillarboxed_view() {
        let rect = aspect_fit_video_rect(1000.0, 1000.0, 1920, 1080).unwrap();
        assert_eq!(rect.left, 0.0);
        assert_eq!(rect.top, 218.75);
        assert_eq!(rect.width, 1000.0);
        assert_eq!(rect.height, 562.5);
    }
}
