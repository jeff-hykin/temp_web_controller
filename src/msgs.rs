use anyhow::{bail, Result};

pub const TWIST_TYPE: &str = "geometry_msgs.Twist";
pub const IMAGE_TYPE: &str = "sensor_msgs.Image";
pub const COMPRESSED_IMAGE_TYPE: &str = "sensor_msgs.CompressedImage";
pub const TF_TYPE: &str = "tf2_msgs.TFMessage";

// LCM fingerprints, extracted from the generated dimos-lcm python types. A
// publisher that sends the wrong 8 bytes is silently ignored by every dimos
// subscriber, so these must match the .lcm definitions exactly.
pub const TWIST_FINGERPRINT: [u8; 8] = [0x2e, 0x7c, 0x07, 0xd7, 0xcd, 0xf7, 0xe0, 0x27];
pub const IMAGE_FINGERPRINT: [u8; 8] = [0x53, 0x5c, 0xfa, 0xce, 0x1f, 0x4f, 0x57, 0x17];
pub const COMPRESSED_IMAGE_FINGERPRINT: [u8; 8] = [0xb8, 0xd0, 0x11, 0xc1, 0x04, 0x12, 0xb9, 0xa1];
pub const TF_FINGERPRINT: [u8; 8] = [0xc2, 0xb8, 0xa1, 0xc3, 0x3a, 0x89, 0x23, 0xec];

pub fn is_image_type(msg_type: &str) -> bool {
    msg_type == IMAGE_TYPE || msg_type == COMPRESSED_IMAGE_TYPE
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        if self.offset + count > self.data.len() {
            bail!("truncated message");
        }
        let slice = &self.data[self.offset..self.offset + count];
        self.offset += count;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into()?))
    }

    fn string(&mut self) -> Result<String> {
        let length = self.u32()? as usize;
        if length == 0 {
            bail!("zero-length lcm string");
        }
        let raw = self.take(length)?;
        Ok(String::from_utf8_lossy(&raw[..length - 1]).into_owned())
    }

    fn expect_fingerprint(&mut self, expected: &[u8; 8]) -> Result<()> {
        if self.take(8)? != expected {
            bail!("fingerprint mismatch");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Header {
    pub stamp_sec: i32,
    pub stamp_nsec: i32,
    pub frame_id: String,
}

pub struct RawImage {
    pub header: Header,
    pub width: usize,
    pub height: usize,
    pub step: usize,
    pub is_bigendian: u8,
    pub encoding: String,
    pub data: Vec<u8>,
}

pub struct CompressedImage {
    pub header: Header,
    pub format: String,
    pub data: Vec<u8>,
}

pub enum ImageMessage {
    Raw(RawImage),
    Compressed(CompressedImage),
}

/// The leading `seq` is read and dropped: ROS2 headers do not carry one.
fn read_header(reader: &mut Reader) -> Result<Header> {
    reader.i32()?;
    let stamp_sec = reader.i32()?;
    let stamp_nsec = reader.i32()?;
    let frame_id = reader.string()?;
    Ok(Header {
        stamp_sec,
        stamp_nsec,
        frame_id,
    })
}

pub fn decode_image(payload: &[u8]) -> Result<ImageMessage> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&IMAGE_FINGERPRINT)?;
    let data_length = reader.i32()?;
    let header = read_header(&mut reader)?;
    let height = reader.i32()?;
    let width = reader.i32()?;
    let encoding = reader.string()?;
    let is_bigendian = reader.u8()?;
    let step = reader.i32()?;
    if data_length < 0 || height <= 0 || width <= 0 || step < 0 {
        bail!("nonsense image dimensions");
    }
    let data = reader.take(data_length as usize)?.to_vec();
    Ok(ImageMessage::Raw(RawImage {
        header,
        width: width as usize,
        height: height as usize,
        step: step as usize,
        is_bigendian,
        encoding,
        data,
    }))
}

pub fn decode_compressed_image(payload: &[u8]) -> Result<ImageMessage> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&COMPRESSED_IMAGE_FINGERPRINT)?;
    let data_length = reader.i32()?;
    let header = read_header(&mut reader)?;
    let format = reader.string()?;
    if data_length < 0 {
        bail!("negative compressed image length");
    }
    let data = reader.take(data_length as usize)?.to_vec();
    Ok(ImageMessage::Compressed(CompressedImage {
        header,
        format,
        data,
    }))
}

pub fn decode_any_image(msg_type: &str, payload: &[u8]) -> Result<ImageMessage> {
    match msg_type {
        IMAGE_TYPE => decode_image(payload),
        COMPRESSED_IMAGE_TYPE => decode_compressed_image(payload),
        other => bail!("not an image type: {other}"),
    }
}

pub struct TfEdge {
    pub header: Header,
    pub parent: String,
    pub child: String,
    pub translation: [f64; 3],
    pub rotation: [f64; 4],
}

pub fn decode_tf(payload: &[u8]) -> Result<Vec<TfEdge>> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&TF_FINGERPRINT)?;
    let count = reader.i32()?;
    if !(0..=4096).contains(&count) {
        bail!("nonsense transform count");
    }
    let mut edges = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let header = read_header(&mut reader)?;
        let child = reader.string()?;
        let mut translation = [0.0; 3];
        for axis in translation.iter_mut() {
            *axis = reader.f64()?;
        }
        let mut rotation = [0.0; 4];
        for term in rotation.iter_mut() {
            *term = reader.f64()?;
        }
        edges.push(TfEdge {
            parent: header.frame_id.clone(),
            child,
            header,
            translation,
            rotation,
        });
    }
    Ok(edges)
}

pub fn decode_twist(payload: &[u8]) -> Result<([f64; 3], [f64; 3])> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&TWIST_FINGERPRINT)?;
    let mut linear = [0.0; 3];
    let mut angular = [0.0; 3];
    for axis in linear.iter_mut().chain(angular.iter_mut()) {
        *axis = reader.f64()?;
    }
    Ok((linear, angular))
}

pub fn encode_twist(linear: [f64; 3], angular: [f64; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(56);
    out.extend_from_slice(&TWIST_FINGERPRINT);
    for value in linear.iter().chain(angular.iter()) {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twist_matches_python_reference() {
        let encoded = encode_twist([0.0; 3], [0.0; 3]);
        assert_eq!(encoded.len(), 56);
        assert_eq!(hex(&encoded[..8]), "2e7c07d7cdf7e027");
        assert!(encoded[8..].iter().all(|byte| *byte == 0));

        let encoded = encode_twist([0.25, 0.0, 0.0], [0.0, 0.0, -0.5]);
        assert_eq!(&encoded[8..16], &0.25f64.to_be_bytes());
        assert_eq!(&encoded[48..56], &(-0.5f64).to_be_bytes());
    }

    #[test]
    fn image_round_trips() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&IMAGE_FINGERPRINT);
        payload.extend_from_slice(&6i32.to_be_bytes());
        payload.extend_from_slice(&7i32.to_be_bytes());
        payload.extend_from_slice(&1i32.to_be_bytes());
        payload.extend_from_slice(&2i32.to_be_bytes());
        push_string(&mut payload, "camera_link");
        payload.extend_from_slice(&2i32.to_be_bytes());
        payload.extend_from_slice(&3i32.to_be_bytes());
        push_string(&mut payload, "rgb8");
        payload.push(0);
        payload.extend_from_slice(&9i32.to_be_bytes());
        payload.extend_from_slice(&[1, 2, 3, 4, 5, 6]);

        let ImageMessage::Raw(image) = decode_image(&payload).unwrap() else {
            panic!("expected a raw image");
        };
        assert_eq!(image.width, 3);
        assert_eq!(image.height, 2);
        assert_eq!(image.step, 9);
        assert_eq!(image.encoding, "rgb8");
        assert_eq!(image.data, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn tf_round_trips() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TF_FINGERPRINT);
        payload.extend_from_slice(&2i32.to_be_bytes());
        for (parent, child) in [("odom", "base_link"), ("base_link", "camera_link")] {
            payload.extend_from_slice(&0i32.to_be_bytes());
            payload.extend_from_slice(&0i32.to_be_bytes());
            payload.extend_from_slice(&0i32.to_be_bytes());
            push_string(&mut payload, parent);
            push_string(&mut payload, child);
            payload.extend_from_slice(&[0u8; 56]);
        }

        let edges = decode_tf(&payload).unwrap();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].parent, "odom");
        assert_eq!(edges[0].child, "base_link");
        assert_eq!(edges[1].child, "camera_link");
    }

    #[test]
    fn wrong_fingerprint_is_rejected() {
        let mut payload = vec![0u8; 64];
        payload[..8].copy_from_slice(&COMPRESSED_IMAGE_FINGERPRINT);
        assert!(decode_image(&payload).is_err());
    }

    fn push_string(buffer: &mut Vec<u8>, value: &str) {
        buffer.extend_from_slice(&((value.len() + 1) as u32).to_be_bytes());
        buffer.extend_from_slice(value.as_bytes());
        buffer.push(0);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
