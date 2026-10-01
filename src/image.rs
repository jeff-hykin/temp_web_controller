use crate::msgs::{CompressedImage, RawImage};
use jpeg_encoder::{ColorType, Encoder};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Recorded frames are archival, so this sits far above what a live view needs.
const RECORD_JPEG_QUALITY: u8 = 92;

/// How image topics are stored in a recording. `Raw` keeps the frame exactly as
/// it arrived; the rest re-encode it as a `sensor_msgs/CompressedImage`.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Raw,
    Jpeg,
    Png,
    Webp,
    Jpegxl,
}

impl ImageFormat {
    /// The `format` field a `CompressedImage` reader keys off.
    fn label(self) -> &'static str {
        match self {
            ImageFormat::Raw => "raw",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Png => "png",
            ImageFormat::Webp => "webp",
            ImageFormat::Jpegxl => "jxl",
        }
    }
}

/// Pixel layouts the recording encoders share. Recording keeps the source
/// resolution and bit depth — a recording that quietly threw precision away
/// would be worse than a large one.
enum Surface {
    Gray8(Vec<u8>),
    Rgb8(Vec<u8>),
    Rgba8(Vec<u8>),
    Gray16(Vec<u16>),
}

/// Re-encodes a frame for recording. `None` means the format cannot hold these
/// pixels without losing precision — jpeg and webp are 8-bit only, and none of
/// them take 32-bit float depth — so the caller keeps the raw frame instead of
/// writing a degraded one.
pub fn compress(image: &RawImage, format: ImageFormat) -> Option<CompressedImage> {
    let surface = surface(image)?;
    let width = image.width as u32;
    let height = image.height as u32;
    let data = match format {
        ImageFormat::Raw => return None,
        ImageFormat::Jpeg => to_jpeg(&surface, width, height)?,
        ImageFormat::Png => to_png(&surface, width, height)?,
        ImageFormat::Webp => to_webp(&surface, width, height)?,
        ImageFormat::Jpegxl => to_jpegxl(&surface, width, height)?,
    };
    Some(CompressedImage {
        header: image.header.clone(),
        format: format.label().to_owned(),
        data,
    })
}

/// Repacks the rows into a tight buffer, dropping any `step` padding and
/// putting the channels in the order every encoder here expects.
fn surface(image: &RawImage) -> Option<Surface> {
    let bytes_per_pixel = match image.encoding.as_str() {
        "mono8" | "8UC1" => 1,
        "rgb8" | "8UC3" | "bgr8" => 3,
        "rgba8" | "8UC4" | "bgra8" => 4,
        "mono16" | "16UC1" | "depth16" => 2,
        _ => return None,
    };
    let tight = image.width.checked_mul(bytes_per_pixel)?;
    let step = if image.step > 0 { image.step } else { tight };
    if step < tight || image.data.len() < step.checked_mul(image.height)? {
        return None;
    }
    let rows = (0..image.height).map(|row| &image.data[row * step..row * step + tight]);

    if bytes_per_pixel == 2 {
        let big_endian = image.is_bigendian != 0;
        return Some(Surface::Gray16(
            rows.flat_map(|row| row.as_chunks::<2>().0.iter().copied())
                .map(|pair| {
                    if big_endian {
                        u16::from_be_bytes(pair)
                    } else {
                        u16::from_le_bytes(pair)
                    }
                })
                .collect(),
        ));
    }

    let mut packed: Vec<u8> = rows.flatten().copied().collect();
    if image.encoding.starts_with("bgr") {
        for pixel in packed.chunks_exact_mut(bytes_per_pixel) {
            pixel.swap(0, 2);
        }
    }
    match bytes_per_pixel {
        1 => Some(Surface::Gray8(packed)),
        3 => Some(Surface::Rgb8(packed)),
        _ => Some(Surface::Rgba8(packed)),
    }
}

fn to_jpeg(surface: &Surface, width: u32, height: u32) -> Option<Vec<u8>> {
    let (bytes, color) = match surface {
        Surface::Gray8(bytes) => (bytes.as_slice(), ColorType::Luma),
        Surface::Rgb8(bytes) => (bytes.as_slice(), ColorType::Rgb),
        Surface::Rgba8(bytes) => (bytes.as_slice(), ColorType::Rgba),
        Surface::Gray16(_) => return None,
    };
    let mut out = Vec::new();
    Encoder::new(&mut out, RECORD_JPEG_QUALITY)
        .encode(bytes, width as u16, height as u16, color)
        .ok()?;
    Some(out)
}

fn to_png(surface: &Surface, width: u32, height: u32) -> Option<Vec<u8>> {
    let (color, depth, bytes) = match surface {
        Surface::Gray8(b) => (png::ColorType::Grayscale, png::BitDepth::Eight, Cow::from(b)),
        Surface::Rgb8(b) => (png::ColorType::Rgb, png::BitDepth::Eight, Cow::from(b)),
        Surface::Rgba8(b) => (png::ColorType::Rgba, png::BitDepth::Eight, Cow::from(b)),
        // png stores 16-bit samples big-endian regardless of the host.
        Surface::Gray16(values) => (
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen,
            Cow::from(values.iter().flat_map(|value| value.to_be_bytes()).collect::<Vec<u8>>()),
        ),
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(depth);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&bytes).ok()?;
    writer.finish().ok()?;
    Some(out)
}

fn to_webp(surface: &Surface, width: u32, height: u32) -> Option<Vec<u8>> {
    let (bytes, color) = match surface {
        Surface::Gray8(b) => (b.as_slice(), image_webp::ColorType::L8),
        Surface::Rgb8(b) => (b.as_slice(), image_webp::ColorType::Rgb8),
        Surface::Rgba8(b) => (b.as_slice(), image_webp::ColorType::Rgba8),
        Surface::Gray16(_) => return None,
    };
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(bytes, width, height, color)
        .ok()?;
    Some(out)
}

fn to_jpegxl(surface: &Surface, width: u32, height: u32) -> Option<Vec<u8>> {
    use zune_core::colorspace::ColorSpace;
    use zune_core::bit_depth::BitDepth;

    let (bytes, colorspace, depth) = match surface {
        Surface::Gray8(b) => (Cow::from(b), ColorSpace::Luma, BitDepth::Eight),
        Surface::Rgb8(b) => (Cow::from(b), ColorSpace::RGB, BitDepth::Eight),
        Surface::Rgba8(b) => (Cow::from(b), ColorSpace::RGBA, BitDepth::Eight),
        // zune reads 16-bit samples as native-endian byte pairs.
        Surface::Gray16(values) => (
            Cow::from(values.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<u8>>()),
            ColorSpace::Luma,
            BitDepth::Sixteen,
        ),
    };
    let options =
        zune_core::options::EncoderOptions::new(width as usize, height as usize, colorspace, depth);
    let mut out = Vec::new();
    zune_jpegxl::JxlSimpleEncoder::new(&bytes, options)
        .encode(&mut out)
        .ok()?;
    Some(out)
}

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Codec {
    Jpeg,
    Png,
}

/// Sniffed rather than taken from the `format` or `encoding` string, because those
/// are free text and a mislabelled payload should be rejected, not served broken.
fn codec_of(data: &[u8]) -> Option<Codec> {
    if data.starts_with(&[0xFF, 0xD8]) {
        Some(Codec::Jpeg)
    } else if data.starts_with(&PNG_MAGIC) {
        Some(Codec::Png)
    } else {
        None
    }
}

/// dimos's `JpegLcmTransport` sends an ordinary `sensor_msgs/Image` whose `data`
/// is a whole compressed stream and whose `step` is 0, so the label is the only
/// hint that the payload is not pixels.
/// A conformant raw frame fills exactly `height * step` bytes and a codec stream
/// essentially never does, so this settles the question structurally. Magic bytes
/// alone would not: a bright mono8 row can genuinely open with ff d8 ff.
pub fn looks_like_pixels(height: usize, step: usize, length: usize) -> bool {
    step != 0 && height.checked_mul(step) == Some(length)
}

/// Neither the right size for its own dimensions nor a container we recognise, so
/// the frame is truncated, mis-measured, or in a codec we do not sniff. Counted
/// rather than logged per frame, since at 60 Hz a warn line would be unreadable.
pub fn frame_is_unclassifiable(height: usize, step: usize, data: &[u8]) -> bool {
    !looks_like_pixels(height, step, data.len()) && codec_of(data).is_none()
}

fn compressed_codec(image: &RawImage) -> Option<Codec> {
    if !matches!(image.encoding.as_str(), "jpeg" | "jpg" | "png") {
        return None;
    }
    if looks_like_pixels(image.height, image.step, image.data.len()) {
        return None;
    }
    codec_of(&image.data)
}

/// The container name to record such a frame under, since writing it back out as
/// an `Image` would claim a codec stream is a pixel layout and give it `step = 0`.
pub fn container_format(image: &RawImage) -> Option<&'static str> {
    match compressed_codec(image)? {
        Codec::Jpeg => Some("jpeg"),
        Codec::Png => Some("png"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth_image(width: usize, height: usize) -> RawImage {
        RawImage {
            header: crate::msgs::Header { stamp_sec: 0, stamp_nsec: 0, frame_id: String::new() },
            is_bigendian: 0,
            width,
            height,
            // Two bytes of row padding, so a decoder that trusts `step` blindly
            // would smear the depths sideways.
            step: width * 2 + 2,
            encoding: "16UC1".to_owned(),
            data: (0..height)
                .flat_map(|row| {
                    (0..width)
                        .flat_map(move |column| (((row * width + column) * 517) as u16).to_le_bytes())
                        .chain([0xff, 0xff])
                })
                .collect(),
        }
    }

    fn colour_image(width: usize, height: usize) -> RawImage {
        RawImage {
            header: crate::msgs::Header { stamp_sec: 0, stamp_nsec: 0, frame_id: String::new() },
            is_bigendian: 0,
            width,
            height,
            step: width * 3,
            encoding: "rgb8".to_owned(),
            data: (0..width * height * 3).map(|index| ((index * 7) % 251) as u8).collect(),
        }
    }

    /// Mirrors dimos's `JpegLcmTransport`: a plain Image whose data is a JFIF
    /// stream and whose step is 0.
    fn prejpeg_image(width: usize, height: usize) -> RawImage {
        let source = colour_image(width, height);
        let compressed = compress(&source, ImageFormat::Jpeg).unwrap();
        RawImage {
            step: 0,
            encoding: "jpeg".to_owned(),
            data: compressed.data,
            ..source
        }
    }

    /// A plain Image whose data is a png stream, which is how frame-dumping tools
    /// tend to publish.
    fn prepng_image(width: usize, height: usize) -> RawImage {
        let source = colour_image(width, height);
        let compressed = compress(&source, ImageFormat::Png).unwrap();
        RawImage {
            step: 0,
            encoding: "png".to_owned(),
            data: compressed.data,
            ..source
        }
    }

    #[test]
    fn a_jpeg_or_png_stream_in_an_image_is_recorded_under_its_container() {
        assert_eq!(container_format(&prejpeg_image(16, 16)), Some("jpeg"));
        assert_eq!(container_format(&prepng_image(16, 16)), Some("png"));
        assert_eq!(container_format(&colour_image(16, 16)), None);
        let mut mislabelled = prejpeg_image(16, 16);
        mislabelled.data[0] = 0x00;
        assert_eq!(container_format(&mislabelled), None, "a jpeg label without the jfif marker is not trusted");
    }

    #[test]
    fn recording_keeps_an_already_jpeg_frame_as_it_arrived() {
        let image = prejpeg_image(16, 16);
        assert!(compress(&image, ImageFormat::Png).is_none());
        assert!(compress(&image, ImageFormat::Jpeg).is_none());
    }

    fn depths(image: &RawImage) -> Vec<u16> {
        let Some(Surface::Gray16(values)) = surface(image) else {
            panic!("expected a 16-bit surface");
        };
        values
    }

    #[test]
    fn png_keeps_every_depth_sample_exact() {
        let image = depth_image(9, 5);
        let encoded = compress(&image, ImageFormat::Png).unwrap();
        assert_eq!(encoded.format, "png");

        let mut reader = png::Decoder::new(std::io::Cursor::new(&encoded.data))
            .read_info()
            .unwrap();
        assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
        let mut bytes = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        let decoded: Vec<u16> = bytes[..info.buffer_size()]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        assert_eq!(decoded, depths(&image));
    }

    #[test]
    fn jpegxl_keeps_every_depth_sample_exact() {
        let image = depth_image(9, 5);
        let encoded = compress(&image, ImageFormat::Jpegxl).unwrap();
        assert_eq!(encoded.format, "jxl");

        let render = jxl_oxide::JxlImage::builder()
            .read(std::io::Cursor::new(&encoded.data))
            .unwrap()
            .render_frame(0)
            .unwrap();
        let scale = f32::from(u16::MAX);
        let decoded: Vec<u16> = render
            .image_all_channels()
            .buf()
            .iter()
            .map(|value| (value * scale).round() as u16)
            .collect();
        assert_eq!(decoded, depths(&image));
    }

    #[test]
    fn webp_keeps_every_colour_pixel_exact() {
        let image = colour_image(11, 7);
        let encoded = compress(&image, ImageFormat::Webp).unwrap();
        assert_eq!(encoded.format, "webp");

        let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(&encoded.data)).unwrap();
        let mut bytes = vec![0u8; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut bytes).unwrap();
        assert_eq!(decoder.dimensions(), (11, 7));
        assert_eq!(bytes, image.data);
    }

    #[test]
    fn lossy_and_colour_only_formats_refuse_depth() {
        let image = depth_image(9, 5);
        assert!(compress(&image, ImageFormat::Jpeg).is_none());
        assert!(compress(&image, ImageFormat::Webp).is_none());
        assert!(compress(&image, ImageFormat::Raw).is_none());
    }

    #[test]
    fn jpeg_still_encodes_colour() {
        let encoded = compress(&colour_image(16, 16), ImageFormat::Jpeg).unwrap();
        assert_eq!(encoded.format, "jpeg");
        assert_eq!(&encoded.data[..2], &[0xff, 0xd8]);
    }
}
