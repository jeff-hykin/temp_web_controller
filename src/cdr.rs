//! Re-encodes the handful of message types we understand into ROS2 CDR, so the
//! recordings open in Foxglove instead of being an opaque pile of LCM bytes.

use crate::msgs::{self, Header, ImageMessage};

/// Concatenated ros2msg text, which is what `message_encoding: "cdr"` readers
/// expect. Note ROS2 headers have no `seq` field even though the LCM ones do.
const TIME_MSG: &str = "\
================================================================================
MSG: builtin_interfaces/Time
int32 sec
uint32 nanosec
";

const HEADER_MSG: &str = "\
================================================================================
MSG: std_msgs/Header
builtin_interfaces/Time stamp
string frame_id
";

const VECTOR3_MSG: &str = "\
================================================================================
MSG: geometry_msgs/Vector3
float64 x
float64 y
float64 z
";

const POINT_MSG: &str = "\
================================================================================
MSG: geometry_msgs/Point
float64 x
float64 y
float64 z
";

const QUATERNION_MSG: &str = "\
================================================================================
MSG: geometry_msgs/Quaternion
float64 x
float64 y
float64 z
float64 w
";

const POSE_MSG: &str = "\
================================================================================
MSG: geometry_msgs/Pose
geometry_msgs/Point position
geometry_msgs/Quaternion orientation
";

const TWIST_MSG: &str = "\
================================================================================
MSG: geometry_msgs/Twist
geometry_msgs/Vector3 linear
geometry_msgs/Vector3 angular
";

pub struct Encoded {
    pub schema_name: String,
    pub schema_text: String,
    pub data: Vec<u8>,
}

/// `None` means we have no ROS2 mapping, and the caller should fall back to
/// storing the raw LCM bytes.
pub fn to_ros2(msg_type: &str, payload: &[u8]) -> Option<Encoded> {
    match msg_type {
        msgs::IMAGE_TYPE | msgs::COMPRESSED_IMAGE_TYPE => image(msg_type, payload),
        msgs::TWIST_TYPE => twist(payload),
        msgs::TF_TYPE => tf(payload),
        msgs::POINT_CLOUD2_TYPE => point_cloud2(payload),
        msgs::ODOMETRY_TYPE => odometry(payload),
        msgs::POSE_STAMPED_TYPE => pose_stamped(payload),
        msgs::IMU_TYPE => imu(payload),
        msgs::CAMERA_INFO_TYPE => camera_info(payload),
        _ => None,
    }
}

fn image(msg_type: &str, payload: &[u8]) -> Option<Encoded> {
    match msgs::decode_any_image(msg_type, payload).ok()? {
        ImageMessage::Raw(image) => Some(raw_image(&image)),
        ImageMessage::Compressed(image) => Some(compressed_image(&image)),
    }
}

fn raw_image(image: &msgs::RawImage) -> Encoded {
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &image.header);
    writer.u32(image.height as u32);
    writer.u32(image.width as u32);
    writer.string(&image.encoding);
    writer.u8(image.is_bigendian);
    writer.u32(image.step as u32);
    writer.bytes(&image.data);
    Encoded {
        schema_name: "sensor_msgs/msg/Image".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             uint32 height\n\
             uint32 width\n\
             string encoding\n\
             uint8 is_bigendian\n\
             uint32 step\n\
             uint8[] data\n\n{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    }
}

pub fn compressed_image(image: &msgs::CompressedImage) -> Encoded {
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &image.header);
    writer.string(&image.format);
    writer.bytes(&image.data);
    Encoded {
        schema_name: "sensor_msgs/msg/CompressedImage".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             string format\n\
             uint8[] data\n\n{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    }
}

fn twist(payload: &[u8]) -> Option<Encoded> {
    let (linear, angular) = msgs::decode_twist(payload).ok()?;
    let mut writer = CdrWriter::new();
    for value in linear.iter().chain(angular.iter()) {
        writer.f64(*value);
    }
    Some(Encoded {
        schema_name: "geometry_msgs/msg/Twist".into(),
        schema_text: format!(
            "geometry_msgs/Vector3 linear\n\
             geometry_msgs/Vector3 angular\n\n{VECTOR3_MSG}"
        ),
        data: writer.finish(),
    })
}

fn tf(payload: &[u8]) -> Option<Encoded> {
    let edges = msgs::decode_tf(payload).ok()?;
    let mut writer = CdrWriter::new();
    writer.u32(edges.len() as u32);
    for edge in &edges {
        write_header(&mut writer, &edge.header);
        writer.string(&edge.child);
        for value in edge.translation.iter().chain(edge.rotation.iter()) {
            writer.f64(*value);
        }
    }
    Some(Encoded {
        schema_name: "tf2_msgs/msg/TFMessage".into(),
        schema_text: format!(
            "geometry_msgs/TransformStamped[] transforms\n\n\
             ================================================================================\n\
             MSG: geometry_msgs/TransformStamped\n\
             std_msgs/Header header\n\
             string child_frame_id\n\
             geometry_msgs/Transform transform\n\n\
             ================================================================================\n\
             MSG: geometry_msgs/Transform\n\
             geometry_msgs/Vector3 translation\n\
             geometry_msgs/Quaternion rotation\n\
             {VECTOR3_MSG}{QUATERNION_MSG}{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn point_cloud2(payload: &[u8]) -> Option<Encoded> {
    let cloud = msgs::decode_point_cloud2(payload).ok()?;
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &cloud.header);
    writer.u32(cloud.height);
    writer.u32(cloud.width);
    writer.u32(cloud.fields.len() as u32);
    for field in &cloud.fields {
        writer.string(&field.name);
        writer.u32(field.offset);
        writer.u8(field.datatype);
        writer.u32(field.count);
    }
    writer.boolean(cloud.is_bigendian);
    writer.u32(cloud.point_step);
    writer.u32(cloud.row_step);
    writer.bytes(&cloud.data);
    writer.boolean(cloud.is_dense);
    Some(Encoded {
        schema_name: "sensor_msgs/msg/PointCloud2".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             uint32 height\n\
             uint32 width\n\
             sensor_msgs/PointField[] fields\n\
             bool is_bigendian\n\
             uint32 point_step\n\
             uint32 row_step\n\
             uint8[] data\n\
             bool is_dense\n\n\
             ================================================================================\n\
             MSG: sensor_msgs/PointField\n\
             string name\n\
             uint32 offset\n\
             uint8 datatype\n\
             uint32 count\n{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn odometry(payload: &[u8]) -> Option<Encoded> {
    let odom = msgs::decode_odometry(payload).ok()?;
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &odom.header);
    writer.string(&odom.child_frame_id);
    write_pose(&mut writer, &odom.pose);
    writer.f64_array(&odom.pose_covariance);
    writer.f64_array(&odom.twist_linear);
    writer.f64_array(&odom.twist_angular);
    writer.f64_array(&odom.twist_covariance);
    Some(Encoded {
        schema_name: "nav_msgs/msg/Odometry".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             string child_frame_id\n\
             geometry_msgs/PoseWithCovariance pose\n\
             geometry_msgs/TwistWithCovariance twist\n\n\
             ================================================================================\n\
             MSG: geometry_msgs/PoseWithCovariance\n\
             geometry_msgs/Pose pose\n\
             float64[36] covariance\n\n\
             ================================================================================\n\
             MSG: geometry_msgs/TwistWithCovariance\n\
             geometry_msgs/Twist twist\n\
             float64[36] covariance\n\
             {POSE_MSG}{TWIST_MSG}{POINT_MSG}{QUATERNION_MSG}{VECTOR3_MSG}{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn pose_stamped(payload: &[u8]) -> Option<Encoded> {
    let stamped = msgs::decode_pose_stamped(payload).ok()?;
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &stamped.header);
    write_pose(&mut writer, &stamped.pose);
    Some(Encoded {
        schema_name: "geometry_msgs/msg/PoseStamped".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             geometry_msgs/Pose pose\n\
             {POSE_MSG}{POINT_MSG}{QUATERNION_MSG}{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn imu(payload: &[u8]) -> Option<Encoded> {
    let imu = msgs::decode_imu(payload).ok()?;
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &imu.header);
    writer.f64_array(&imu.orientation);
    writer.f64_array(&imu.orientation_covariance);
    writer.f64_array(&imu.angular_velocity);
    writer.f64_array(&imu.angular_velocity_covariance);
    writer.f64_array(&imu.linear_acceleration);
    writer.f64_array(&imu.linear_acceleration_covariance);
    Some(Encoded {
        schema_name: "sensor_msgs/msg/Imu".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             geometry_msgs/Quaternion orientation\n\
             float64[9] orientation_covariance\n\
             geometry_msgs/Vector3 angular_velocity\n\
             float64[9] angular_velocity_covariance\n\
             geometry_msgs/Vector3 linear_acceleration\n\
             float64[9] linear_acceleration_covariance\n\
             {QUATERNION_MSG}{VECTOR3_MSG}{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn camera_info(payload: &[u8]) -> Option<Encoded> {
    let info = msgs::decode_camera_info(payload).ok()?;
    let mut writer = CdrWriter::new();
    write_header(&mut writer, &info.header);
    writer.u32(info.height);
    writer.u32(info.width);
    writer.string(&info.distortion_model);
    writer.u32(info.distortion.len() as u32);
    writer.f64_array(&info.distortion);
    writer.f64_array(&info.intrinsics);
    writer.f64_array(&info.rectification);
    writer.f64_array(&info.projection);
    writer.u32(info.binning_x);
    writer.u32(info.binning_y);
    writer.u32(info.roi.x_offset);
    writer.u32(info.roi.y_offset);
    writer.u32(info.roi.height);
    writer.u32(info.roi.width);
    writer.boolean(info.roi.do_rectify);
    Some(Encoded {
        schema_name: "sensor_msgs/msg/CameraInfo".into(),
        schema_text: format!(
            "std_msgs/Header header\n\
             uint32 height\n\
             uint32 width\n\
             string distortion_model\n\
             float64[] d\n\
             float64[9] k\n\
             float64[9] r\n\
             float64[12] p\n\
             uint32 binning_x\n\
             uint32 binning_y\n\
             sensor_msgs/RegionOfInterest roi\n\n\
             ================================================================================\n\
             MSG: sensor_msgs/RegionOfInterest\n\
             uint32 x_offset\n\
             uint32 y_offset\n\
             uint32 height\n\
             uint32 width\n\
             bool do_rectify\n{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
}

fn write_pose(writer: &mut CdrWriter, pose: &msgs::Pose) {
    writer.f64_array(&pose.position);
    writer.f64_array(&pose.orientation);
}

fn write_header(writer: &mut CdrWriter, header: &Header) {
    writer.i32(header.stamp_sec);
    writer.u32(header.stamp_nsec as u32);
    writer.string(&header.frame_id);
}

struct CdrWriter {
    buffer: Vec<u8>,
}

impl CdrWriter {
    fn new() -> Self {
        // Encapsulation header: little-endian CDR, no options.
        CdrWriter {
            buffer: vec![0x00, 0x01, 0x00, 0x00],
        }
    }

    /// CDR alignment is measured from the start of the body, not the file, so
    /// the four header bytes do not count.
    fn align(&mut self, width: usize) {
        let body = self.buffer.len() - 4;
        let padding = (width - (body % width)) % width;
        self.buffer.resize(self.buffer.len() + padding, 0);
    }

    fn u8(&mut self, value: u8) {
        self.buffer.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.align(4);
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.align(4);
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn f64(&mut self, value: f64) {
        self.align(8);
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn boolean(&mut self, value: bool) {
        self.u8(value as u8);
    }

    /// No length prefix: in CDR a fixed-size array is just its elements, and a
    /// variable-length one gets its count written separately by the caller.
    fn f64_array(&mut self, values: &[f64]) {
        for value in values {
            self.f64(*value);
        }
    }

    fn string(&mut self, value: &str) {
        self.u32(value.len() as u32 + 1);
        self.buffer.extend_from_slice(value.as_bytes());
        self.buffer.push(0);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.buffer.extend_from_slice(value);
    }

    fn finish(self) -> Vec<u8> {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twist_lands_as_six_aligned_doubles() {
        let payload = msgs::encode_twist([0.25, 0.0, 0.0], [0.0, 0.0, -0.5]);
        let encoded = to_ros2(msgs::TWIST_TYPE, &payload).unwrap();
        assert_eq!(encoded.schema_name, "geometry_msgs/msg/Twist");
        assert_eq!(encoded.data.len(), 4 + 48);
        assert_eq!(&encoded.data[..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&encoded.data[4..12], &0.25f64.to_le_bytes());
        assert_eq!(&encoded.data[44..52], &(-0.5f64).to_le_bytes());
    }

    #[test]
    fn strings_pad_the_next_field_back_to_alignment() {
        let mut writer = CdrWriter::new();
        writer.string("ab");
        // 4 length bytes + "ab\0" leaves the body at 7.
        assert_eq!(writer.buffer.len() - 4, 7);
        writer.u32(1);
        assert_eq!(writer.buffer.len() - 4, 12);
    }

    #[test]
    fn unknown_types_have_no_mapping() {
        assert!(to_ros2("std_msgs.Float64MultiArray", &[0u8; 32]).is_none());
    }

    #[test]
    fn a_mapped_type_with_undecodable_bytes_still_declines() {
        assert!(to_ros2(msgs::POINT_CLOUD2_TYPE, &[7u8; 32]).is_none());
        assert!(to_ros2(msgs::IMU_TYPE, &[7u8; 32]).is_none());
    }

    #[test]
    fn pose_stamped_is_a_header_then_seven_doubles() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&msgs::POSE_STAMPED_FINGERPRINT);
        payload.extend_from_slice(&0i32.to_be_bytes());
        payload.extend_from_slice(&7i32.to_be_bytes());
        payload.extend_from_slice(&8i32.to_be_bytes());
        payload.extend_from_slice(&5u32.to_be_bytes());
        payload.extend_from_slice(b"odom\0");
        for value in [20.0f64, 21.0, 22.0, 0.0, 0.0, 0.0, 1.0] {
            payload.extend_from_slice(&value.to_be_bytes());
        }

        let encoded = to_ros2(msgs::POSE_STAMPED_TYPE, &payload).unwrap();
        assert_eq!(encoded.schema_name, "geometry_msgs/msg/PoseStamped");
        let body = &encoded.data[4..];
        assert_eq!(&body[..4], &7i32.to_le_bytes());
        assert_eq!(&body[4..8], &8u32.to_le_bytes());
        assert_eq!(&body[8..12], &5u32.to_le_bytes());
        assert_eq!(&body[12..17], b"odom\0");
        // "odom\0" leaves the body at 17, so the first double realigns to 24.
        assert_eq!(&body[24..32], &20.0f64.to_le_bytes());
        assert_eq!(&body[72..80], &1.0f64.to_le_bytes());
        assert_eq!(body.len(), 80);
    }
}
