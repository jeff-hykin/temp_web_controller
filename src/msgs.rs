use anyhow::{bail, Result};

pub const TWIST_TYPE: &str = "geometry_msgs.Twist";
pub const IMAGE_TYPE: &str = "sensor_msgs.Image";
pub const COMPRESSED_IMAGE_TYPE: &str = "sensor_msgs.CompressedImage";
pub const TF_TYPE: &str = "tf2_msgs.TFMessage";
pub const POINT_CLOUD2_TYPE: &str = "sensor_msgs.PointCloud2";
pub const IMU_TYPE: &str = "sensor_msgs.Imu";
pub const CAMERA_INFO_TYPE: &str = "sensor_msgs.CameraInfo";
pub const ODOMETRY_TYPE: &str = "nav_msgs.Odometry";
pub const POSE_STAMPED_TYPE: &str = "geometry_msgs.PoseStamped";

// LCM fingerprints, extracted from the generated dimos-lcm python types. A
// publisher that sends the wrong 8 bytes is silently ignored by every dimos
// subscriber, so these must match the .lcm definitions exactly.
pub const TWIST_FINGERPRINT: [u8; 8] = [0x2e, 0x7c, 0x07, 0xd7, 0xcd, 0xf7, 0xe0, 0x27];
pub const IMAGE_FINGERPRINT: [u8; 8] = [0x53, 0x5c, 0xfa, 0xce, 0x1f, 0x4f, 0x57, 0x17];
pub const COMPRESSED_IMAGE_FINGERPRINT: [u8; 8] = [0xb8, 0xd0, 0x11, 0xc1, 0x04, 0x12, 0xb9, 0xa1];
pub const TF_FINGERPRINT: [u8; 8] = [0xc2, 0xb8, 0xa1, 0xc3, 0x3a, 0x89, 0x23, 0xec];
pub const POINT_CLOUD2_FINGERPRINT: [u8; 8] = [0xf5, 0xeb, 0x3d, 0xa1, 0xc2, 0x85, 0x31, 0x75];
pub const IMU_FINGERPRINT: [u8; 8] = [0x31, 0xab, 0x20, 0x5c, 0x8d, 0xd5, 0x7a, 0xa8];
pub const CAMERA_INFO_FINGERPRINT: [u8; 8] = [0xaf, 0xb5, 0x01, 0xb0, 0x07, 0x07, 0xa0, 0x2a];
pub const ODOMETRY_FINGERPRINT: [u8; 8] = [0x94, 0xe1, 0x7f, 0x94, 0x64, 0x8c, 0xf0, 0xe4];
pub const POSE_STAMPED_FINGERPRINT: [u8; 8] = [0x6a, 0x82, 0x69, 0x64, 0x58, 0xc2, 0x79, 0xa0];

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

    /// LCM writes `boolean` as one byte.
    fn boolean(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    fn f64_array<const N: usize>(&mut self) -> Result<[f64; N]> {
        let mut values = [0.0; N];
        for value in values.iter_mut() {
            *value = self.f64()?;
        }
        Ok(values)
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

/// A `RawImage` whose pixels are still borrowed, so the per-frame health check can
/// look at the shape of a megabyte without copying it.
pub struct RawImageParts<'a> {
    pub header: Header,
    pub width: usize,
    pub height: usize,
    pub step: usize,
    pub is_bigendian: u8,
    pub encoding: String,
    pub data: &'a [u8],
}

pub fn read_raw_image(payload: &[u8]) -> Result<RawImageParts<'_>> {
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
    Ok(RawImageParts {
        header,
        width: width as usize,
        height: height as usize,
        step: step as usize,
        is_bigendian,
        encoding,
        data: reader.take(data_length as usize)?,
    })
}

pub fn decode_image(payload: &[u8]) -> Result<ImageMessage> {
    let parts = read_raw_image(payload)?;
    Ok(ImageMessage::Raw(RawImage {
        header: parts.header,
        width: parts.width,
        height: parts.height,
        step: parts.step,
        is_bigendian: parts.is_bigendian,
        encoding: parts.encoding,
        data: parts.data.to_vec(),
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

pub struct Pose {
    pub position: [f64; 3],
    pub orientation: [f64; 4],
}

fn read_pose(reader: &mut Reader) -> Result<Pose> {
    Ok(Pose {
        position: reader.f64_array()?,
        orientation: reader.f64_array()?,
    })
}

pub struct PointField {
    pub name: String,
    pub offset: u32,
    pub datatype: u8,
    pub count: u32,
}

pub struct PointCloud2 {
    pub header: Header,
    pub height: u32,
    pub width: u32,
    pub fields: Vec<PointField>,
    pub is_bigendian: bool,
    pub point_step: u32,
    pub row_step: u32,
    pub data: Vec<u8>,
    pub is_dense: bool,
}

/// Array lengths come first in the dimos LCM structs, ahead of the header, which
/// is the one place these layouts diverge from the ROS2 ones they mirror.
pub fn decode_point_cloud2(payload: &[u8]) -> Result<PointCloud2> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&POINT_CLOUD2_FINGERPRINT)?;
    let fields_length = reader.i32()?;
    let data_length = reader.i32()?;
    if !(0..=1024).contains(&fields_length) || data_length < 0 {
        bail!("nonsense point cloud lengths");
    }
    let header = read_header(&mut reader)?;
    let height = reader.i32()?;
    let width = reader.i32()?;
    let mut fields = Vec::with_capacity(fields_length as usize);
    for _ in 0..fields_length {
        fields.push(PointField {
            name: reader.string()?,
            offset: reader.i32()? as u32,
            datatype: reader.u8()?,
            count: reader.i32()? as u32,
        });
    }
    let is_bigendian = reader.boolean()?;
    let point_step = reader.i32()? as u32;
    let row_step = reader.i32()? as u32;
    let data = reader.take(data_length as usize)?.to_vec();
    let is_dense = reader.boolean()?;
    Ok(PointCloud2 {
        header,
        height: height as u32,
        width: width as u32,
        fields,
        is_bigendian,
        point_step,
        row_step,
        data,
        is_dense,
    })
}

pub struct Odometry {
    pub header: Header,
    pub child_frame_id: String,
    pub pose: Pose,
    pub pose_covariance: [f64; 36],
    pub twist_linear: [f64; 3],
    pub twist_angular: [f64; 3],
    pub twist_covariance: [f64; 36],
}

pub fn decode_odometry(payload: &[u8]) -> Result<Odometry> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&ODOMETRY_FINGERPRINT)?;
    Ok(Odometry {
        header: read_header(&mut reader)?,
        child_frame_id: reader.string()?,
        pose: read_pose(&mut reader)?,
        pose_covariance: reader.f64_array()?,
        twist_linear: reader.f64_array()?,
        twist_angular: reader.f64_array()?,
        twist_covariance: reader.f64_array()?,
    })
}

pub struct PoseStamped {
    pub header: Header,
    pub pose: Pose,
}

pub fn decode_pose_stamped(payload: &[u8]) -> Result<PoseStamped> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&POSE_STAMPED_FINGERPRINT)?;
    Ok(PoseStamped {
        header: read_header(&mut reader)?,
        pose: read_pose(&mut reader)?,
    })
}

pub struct Imu {
    pub header: Header,
    pub orientation: [f64; 4],
    pub orientation_covariance: [f64; 9],
    pub angular_velocity: [f64; 3],
    pub angular_velocity_covariance: [f64; 9],
    pub linear_acceleration: [f64; 3],
    pub linear_acceleration_covariance: [f64; 9],
}

pub fn decode_imu(payload: &[u8]) -> Result<Imu> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&IMU_FINGERPRINT)?;
    Ok(Imu {
        header: read_header(&mut reader)?,
        orientation: reader.f64_array()?,
        orientation_covariance: reader.f64_array()?,
        angular_velocity: reader.f64_array()?,
        angular_velocity_covariance: reader.f64_array()?,
        linear_acceleration: reader.f64_array()?,
        linear_acceleration_covariance: reader.f64_array()?,
    })
}

pub struct RegionOfInterest {
    pub x_offset: u32,
    pub y_offset: u32,
    pub height: u32,
    pub width: u32,
    pub do_rectify: bool,
}

pub struct CameraInfo {
    pub header: Header,
    pub height: u32,
    pub width: u32,
    pub distortion_model: String,
    pub distortion: Vec<f64>,
    pub intrinsics: [f64; 9],
    pub rectification: [f64; 9],
    pub projection: [f64; 12],
    pub binning_x: u32,
    pub binning_y: u32,
    pub roi: RegionOfInterest,
}

pub fn decode_camera_info(payload: &[u8]) -> Result<CameraInfo> {
    let mut reader = Reader::new(payload);
    reader.expect_fingerprint(&CAMERA_INFO_FINGERPRINT)?;
    let distortion_length = reader.i32()?;
    if !(0..=64).contains(&distortion_length) {
        bail!("nonsense distortion coefficient count");
    }
    let header = read_header(&mut reader)?;
    let height = reader.i32()? as u32;
    let width = reader.i32()? as u32;
    let distortion_model = reader.string()?;
    let mut distortion = Vec::with_capacity(distortion_length as usize);
    for _ in 0..distortion_length {
        distortion.push(reader.f64()?);
    }
    Ok(CameraInfo {
        header,
        height,
        width,
        distortion_model,
        distortion,
        intrinsics: reader.f64_array()?,
        rectification: reader.f64_array()?,
        projection: reader.f64_array()?,
        binning_x: reader.i32()? as u32,
        binning_y: reader.i32()? as u32,
        roi: RegionOfInterest {
            x_offset: reader.i32()? as u32,
            y_offset: reader.i32()? as u32,
            height: reader.i32()? as u32,
            width: reader.i32()? as u32,
            do_rectify: reader.boolean()?,
        },
    })
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

    /// Captured from `lcm_msgs.sensor_msgs.PointCloud2.lcm_encode()`, so this
    /// pins the real wire layout rather than my reading of it: three xyz+intensity
    /// points, frame "map", with the two array lengths ahead of the header.
    const REAL_POINT_CLOUD: &str = "\
        f5eb3da1c285317500000004000000300000000b6a8e2320075bcd15000000046d6170000000\
        0001000000030000000278000000000007000000010000000279000000000407000000010000\
        00027a000000000807000000010000000a696e74656e73697479000000000c07000000010000\
        000010000000300000c03f00002040000060400000803e000080c00000a0400000c0c0000040\
        3f0000e04000000841000014410000803f01";

    #[test]
    fn point_cloud_decodes_a_real_dimos_payload() {
        let cloud = decode_point_cloud2(&unhex(REAL_POINT_CLOUD)).unwrap();
        assert_eq!(cloud.header.frame_id, "map");
        assert_eq!(cloud.header.stamp_sec, 1787700000);
        assert_eq!(cloud.header.stamp_nsec, 123456789);
        assert_eq!((cloud.height, cloud.width), (1, 3));
        assert_eq!((cloud.point_step, cloud.row_step), (16, 48));
        assert!(!cloud.is_bigendian && cloud.is_dense);

        let names: Vec<&str> = cloud.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["x", "y", "z", "intensity"]);
        assert!(cloud.fields.iter().all(|f| f.datatype == 7 && f.count == 1));
        assert_eq!(cloud.fields[3].offset, 12);

        assert_eq!(cloud.data.len(), 48);
        assert_eq!(&cloud.data[..4], &1.5f32.to_le_bytes());
        assert_eq!(&cloud.data[44..], &1.0f32.to_le_bytes());
    }

    #[test]
    fn camera_info_reads_its_leading_array_length() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&CAMERA_INFO_FINGERPRINT);
        payload.extend_from_slice(&2i32.to_be_bytes());
        push_header(&mut payload, "camera_link");
        payload.extend_from_slice(&480i32.to_be_bytes());
        payload.extend_from_slice(&640i32.to_be_bytes());
        push_string(&mut payload, "plumb_bob");
        for value in [0.5f64, -0.25] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        for index in 0..30 {
            payload.extend_from_slice(&(index as f64).to_be_bytes());
        }
        payload.extend_from_slice(&2i32.to_be_bytes());
        payload.extend_from_slice(&3i32.to_be_bytes());
        for value in [4i32, 5, 6, 7] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        payload.push(1);

        let info = decode_camera_info(&payload).unwrap();
        assert_eq!(info.distortion, vec![0.5, -0.25]);
        assert_eq!(info.distortion_model, "plumb_bob");
        assert_eq!((info.height, info.width), (480, 640));
        assert_eq!(info.intrinsics[0], 0.0);
        assert_eq!(info.rectification[0], 9.0);
        assert_eq!(info.projection[11], 29.0);
        assert_eq!((info.binning_x, info.binning_y), (2, 3));
        assert_eq!(info.roi.width, 7);
        assert!(info.roi.do_rectify);
    }

    #[test]
    fn odometry_keeps_pose_and_twist_covariances_apart() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&ODOMETRY_FINGERPRINT);
        push_header(&mut payload, "odom");
        push_string(&mut payload, "base_link");
        for value in [10.0f64, 11.0, 12.0, 0.0, 0.0, 0.0, 1.0] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        for index in 0..36 {
            payload.extend_from_slice(&(index as f64).to_be_bytes());
        }
        for value in [0.5f64, 0.0, 0.0, 0.0, 0.0, -0.25] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        for index in 0..36 {
            payload.extend_from_slice(&((100 + index) as f64).to_be_bytes());
        }

        let odom = decode_odometry(&payload).unwrap();
        assert_eq!(odom.child_frame_id, "base_link");
        assert_eq!(odom.pose.position, [10.0, 11.0, 12.0]);
        assert_eq!(odom.pose.orientation[3], 1.0);
        assert_eq!(odom.pose_covariance[35], 35.0);
        assert_eq!(odom.twist_linear[0], 0.5);
        assert_eq!(odom.twist_angular[2], -0.25);
        assert_eq!(odom.twist_covariance[0], 100.0);
    }

    #[test]
    fn a_truncated_point_cloud_is_an_error_not_a_panic() {
        let full = unhex(REAL_POINT_CLOUD);
        for length in [8, 20, 60, full.len() - 1] {
            assert!(decode_point_cloud2(&full[..length]).is_err());
        }
    }

    #[test]
    fn wrong_fingerprint_is_rejected() {
        let mut payload = vec![0u8; 64];
        payload[..8].copy_from_slice(&COMPRESSED_IMAGE_FINGERPRINT);
        assert!(decode_image(&payload).is_err());
    }

    fn unhex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
        digits
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    /// seq, sec, nsec, frame_id — the LCM shape, with the `seq` ROS2 lacks.
    fn push_header(buffer: &mut Vec<u8>, frame_id: &str) {
        buffer.extend_from_slice(&0i32.to_be_bytes());
        buffer.extend_from_slice(&0i32.to_be_bytes());
        buffer.extend_from_slice(&0i32.to_be_bytes());
        push_string(buffer, frame_id);
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
