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
             geometry_msgs/Quaternion rotation\n\n{VECTOR3_MSG}\n\
             ================================================================================\n\
             MSG: geometry_msgs/Quaternion\n\
             float64 x\n\
             float64 y\n\
             float64 z\n\
             float64 w\n{HEADER_MSG}\n{TIME_MSG}"
        ),
        data: writer.finish(),
    })
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
        assert!(to_ros2("nav_msgs.Odometry", &[0u8; 32]).is_none());
    }
}
