use crate::msgs::RawImage;
use anyhow::{bail, Result};
use bytes::Bytes;
use jpeg_encoder::{ColorType, Encoder};

pub struct EncodedFrame {
    pub jpeg: Bytes,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Pixels {
    Gray,
    Rgb,
}

fn kind_of(encoding: &str) -> Option<(Pixels, usize, bool)> {
    let (pixels, bytes_per_pixel, is_depth) = match encoding {
        "mono8" | "8UC1" => (Pixels::Gray, 1, false),
        "rgb8" | "8UC3" => (Pixels::Rgb, 3, false),
        "bgr8" => (Pixels::Rgb, 3, false),
        "rgba8" | "8UC4" => (Pixels::Rgb, 4, false),
        "bgra8" => (Pixels::Rgb, 4, false),
        "mono16" | "16UC1" | "depth16" => (Pixels::Rgb, 2, true),
        "32FC1" => (Pixels::Rgb, 4, true),
        _ => return None,
    };
    Some((pixels, bytes_per_pixel, is_depth))
}

/// Downscale and colour-convert in one pass, then JPEG encode.
///
/// Sampling is nearest neighbour: at the frame rates this streams, a cheap
/// resample that keeps up beats a pretty one that forces frames to be dropped.
pub fn encode(image: &RawImage, quality: u8, max_width: usize) -> Result<EncodedFrame> {
    let Some((pixels, bytes_per_pixel, is_depth)) = kind_of(&image.encoding) else {
        bail!("unsupported image encoding: {}", image.encoding);
    };
    let step = if image.step > 0 {
        image.step
    } else {
        image.width * bytes_per_pixel
    };
    if image.data.len() < step * image.height {
        bail!("image payload is shorter than height * step");
    }

    let (width, height) = fit(image.width, image.height, max_width);
    let channels = if pixels == Pixels::Gray { 1 } else { 3 };
    let mut out = vec![0u8; width * height * channels];

    if is_depth {
        let samples = sample_depth(image, step, bytes_per_pixel, width, height);
        let (low, high) = span(&samples);
        for (index, value) in samples.iter().enumerate() {
            let color = match value {
                Some(value) => turbo((value - low) / (high - low)),
                None => [0, 0, 0],
            };
            out[index * 3..index * 3 + 3].copy_from_slice(&color);
        }
    } else {
        let swap_red_blue = image.encoding.starts_with("bgr");
        for y in 0..height {
            let source_row = y * image.height / height;
            for x in 0..width {
                let source_column = x * image.width / width;
                let source = source_row * step + source_column * bytes_per_pixel;
                let target = (y * width + x) * channels;
                if channels == 1 {
                    out[target] = image.data[source];
                } else if swap_red_blue {
                    out[target] = image.data[source + 2];
                    out[target + 1] = image.data[source + 1];
                    out[target + 2] = image.data[source];
                } else {
                    out[target..target + 3].copy_from_slice(&image.data[source..source + 3]);
                }
            }
        }
    }

    let color_type = if channels == 1 {
        ColorType::Luma
    } else {
        ColorType::Rgb
    };
    let mut jpeg = Vec::with_capacity(width * height / 4);
    Encoder::new(&mut jpeg, quality).encode(&out, width as u16, height as u16, color_type)?;

    Ok(EncodedFrame {
        jpeg: Bytes::from(jpeg),
        width,
        height,
    })
}

fn fit(width: usize, height: usize, max_width: usize) -> (usize, usize) {
    if width <= max_width || max_width == 0 {
        return (width.max(1), height.max(1));
    }
    let scaled_height = height * max_width / width.max(1);
    (max_width, scaled_height.max(1))
}

fn sample_depth(
    image: &RawImage,
    step: usize,
    bytes_per_pixel: usize,
    width: usize,
    height: usize,
) -> Vec<Option<f32>> {
    let mut samples = Vec::with_capacity(width * height);
    for y in 0..height {
        let source_row = y * image.height / height;
        for x in 0..width {
            let source_column = x * image.width / width;
            let source = source_row * step + source_column * bytes_per_pixel;
            let value = if bytes_per_pixel == 2 {
                let raw = u16::from_le_bytes([image.data[source], image.data[source + 1]]);
                if raw == 0 {
                    None
                } else {
                    Some(raw as f32)
                }
            } else {
                let raw = f32::from_le_bytes(image.data[source..source + 4].try_into().unwrap());
                if raw.is_finite() && raw > 0.0 {
                    Some(raw)
                } else {
                    None
                }
            };
            samples.push(value);
        }
    }
    samples
}

fn span(samples: &[Option<f32>]) -> (f32, f32) {
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    for value in samples.iter().flatten() {
        low = low.min(*value);
        high = high.max(*value);
    }
    if low >= high {
        return (0.0, 1.0);
    }
    (low, high)
}

fn turbo(position: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 6] = [
        [0.19, 0.07, 0.23],
        [0.11, 0.53, 0.90],
        [0.14, 0.87, 0.68],
        [0.68, 0.98, 0.24],
        [0.98, 0.68, 0.12],
        [0.73, 0.09, 0.03],
    ];
    let position = position.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let index = position.floor() as usize;
    let next = (index + 1).min(STOPS.len() - 1);
    let blend = position - index as f32;
    let mut color = [0u8; 3];
    for channel in 0..3 {
        let value = STOPS[index][channel] * (1.0 - blend) + STOPS[next][channel] * blend;
        color[channel] = (value * 255.0).clamp(0.0, 255.0) as u8;
    }
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_image(width: usize, height: usize) -> RawImage {
        RawImage {
            header: crate::msgs::Header { stamp_sec: 0, stamp_nsec: 0, frame_id: String::new() },
            is_bigendian: 0,
            width,
            height,
            step: width,
            encoding: "mono8".to_owned(),
            data: (0..width * height).map(|index| index as u8).collect(),
        }
    }

    #[test]
    fn downscales_to_the_requested_width() {
        let frame = encode(&gray_image(640, 480), 70, 320).unwrap();
        assert_eq!((frame.width, frame.height), (320, 240));
        assert_eq!(&frame.jpeg[..2], &[0xff, 0xd8]);
    }

    #[test]
    fn keeps_small_images_at_native_size() {
        let frame = encode(&gray_image(64, 48), 70, 320).unwrap();
        assert_eq!((frame.width, frame.height), (64, 48));
    }

    #[test]
    fn lower_quality_produces_smaller_jpegs() {
        let mut noisy = gray_image(320, 240);
        for (index, byte) in noisy.data.iter_mut().enumerate() {
            *byte = ((index * 37) % 251) as u8;
        }
        let high = encode(&noisy, 90, 320).unwrap().jpeg.len();
        let low = encode(&noisy, 25, 320).unwrap().jpeg.len();
        assert!(low < high, "quality 25 ({low}) should beat quality 90 ({high})");
    }

    #[test]
    fn depth_frames_become_colour() {
        let image = RawImage {
            header: crate::msgs::Header { stamp_sec: 0, stamp_nsec: 0, frame_id: String::new() },
            is_bigendian: 0,
            width: 4,
            height: 2,
            step: 8,
            encoding: "16UC1".to_owned(),
            data: (0..8u16).flat_map(|value| (value * 100).to_le_bytes()).collect(),
        };
        let frame = encode(&image, 80, 640).unwrap();
        assert_eq!((frame.width, frame.height), (4, 2));
    }

    #[test]
    fn unknown_encodings_are_rejected() {
        let mut image = gray_image(8, 8);
        image.encoding = "yuv422".to_owned();
        assert!(encode(&image, 70, 320).is_err());
    }
}
