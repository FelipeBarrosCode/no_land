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
    use super::{map_to_video, VideoRect};

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
}
