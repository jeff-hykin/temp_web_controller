"""Read-only camera relay for the R1 test: ROS2 CompressedImage (vendor head camera) -> dimos Image(encoding="jpeg") on LCM.

Subscribes to camera topics only and publishes only image channels; it never touches a command topic.
Run with the dimos venv python plus the ROS2 python path, in the vendor ROS env:
    source /opt/ros/humble/setup.bash; source ~/galaxea-dimos/install/setup.bash
    ROS_DOMAIN_ID=01 ROS_LOCALHOST_ONLY=1 PYTHONPATH=$PYTHONPATH ~/dimos/.venv/bin/python camera_relay.py
"""

import os
import sys
import threading
import time
import zlib

import lcm
import rclpy
from dimos_lcm.sensor_msgs import Image
from rclpy.node import Node
from rclpy.qos import qos_profile_sensor_data
from sensor_msgs.msg import CompressedImage

LCM_URL = os.environ.get("RELAY_LCM_URL", "udpm://239.255.76.67:7767?ttl=0")
# Optional per-frame log for latency measurement (test/r1/measure.js):
# `channel crc32 bytes camera_stamp received_unix_time published_unix_time marker`.
FRAME_LOG = os.environ.get("RELAY_FRAME_LOG")
# Optional: channels (comma-separated substrings) whose frames get a frame counter drawn into the pixels, so a
# browser can tell exactly which frame it shows after any re-encode (web_ctrl's jpeg, zenoh-web's H.264).
# Costs a decode + encode per frame (~25 ms on an Orin core), so leave it off unless measuring.
MARK_CHANNELS = [name for name in os.environ.get("RELAY_MARK", "").split(",") if name]
MARK_QUALITY = int(os.environ.get("RELAY_MARK_QUALITY", "80"))
# Marker geometry, as fractions of the frame (measure.js reads the same): a top-left strip of 18 square
# blocks, white and black guards then 16 bits of the counter, most significant first.
MARK_BLOCKS = 18
MARK_BLOCK_FRACTION = 0.05
ROUTES = {
    "/hdas/camera_head/left_raw/image_raw_color/compressed": "/r1_head_left#sensor_msgs.Image",
    "/hdas/camera_head/right_raw/image_raw_color/compressed": "/r1_head_right#sensor_msgs.Image",
}


def jpeg_size(data):
    """Width and height from a JPEG's SOF marker, or (0, 0)."""
    index = 2
    while index + 9 < len(data):
        if data[index] != 0xFF:
            index += 1
            continue
        marker = data[index + 1]
        length = (data[index + 2] << 8) | data[index + 3]
        if marker in (0xC0, 0xC1, 0xC2):
            height = (data[index + 5] << 8) | data[index + 6]
            width = (data[index + 7] << 8) | data[index + 8]
            return width, height
        index += 2 + length
    return 0, 0


class CameraRelay(Node):
    def __init__(self):
        super().__init__("zenoh_web_test_camera_relay")
        self.lcm = lcm.LCM(LCM_URL)
        self.counts = {channel: 0 for channel in ROUTES.values()}
        for topic, channel in ROUTES.items():
            self.create_subscription(
                CompressedImage, topic, lambda message, channel=channel: self.relay(message, channel), qos_profile_sensor_data
            )
        self.frame_log = open(FRAME_LOG, "a", buffering=1) if FRAME_LOG else None
        self.marked = {channel: 0 for channel in ROUTES.values()}
        self.turbo = None
        self.publish_lock = threading.Lock()
        # Marking runs on its own thread (turbojpeg releases the GIL), newest frame wins, so the
        # unmarked cameras are never held up behind it.
        self.mark_slot = None
        self.mark_ready = threading.Condition()
        if MARK_CHANNELS:
            from turbojpeg import TurboJPEG

            self.turbo = TurboJPEG()
            threading.Thread(target=self.mark_worker, daemon=True).start()
        self.started = time.time()
        self.create_timer(5.0, self.report)

    def relay(self, message, channel):
        if "jpeg" not in message.format.lower() and "jpg" not in message.format.lower():
            return
        received_at = time.time()
        if self.turbo and any(name in channel for name in MARK_CHANNELS):
            with self.mark_ready:
                self.mark_slot = (message, channel, received_at)
                self.mark_ready.notify()
            return
        self.publish(message, channel, bytes(message.data), received_at, -1)

    def mark_worker(self):
        while True:
            with self.mark_ready:
                while self.mark_slot is None:
                    self.mark_ready.wait()
                message, channel, received_at = self.mark_slot
                self.mark_slot = None
            marker = self.marked[channel] & 0xFFFF
            self.marked[channel] += 1
            self.publish(message, channel, self.mark(bytes(message.data), marker), received_at, marker)

    def publish(self, message, channel, data, received_at, marker):
        image = Image()
        image.header.stamp.sec = message.header.stamp.sec
        image.header.stamp.nsec = message.header.stamp.nanosec
        image.header.frame_id = message.header.frame_id
        image.width, image.height = jpeg_size(data)
        image.encoding = "jpeg"
        image.is_bigendian = 0
        image.step = 0
        image.data_length = len(data)
        image.data = data
        encoded = image.lcm_encode()
        with self.publish_lock:
            published_at = time.time()
            self.lcm.publish(channel, encoded)
            self.counts[channel] += 1
            if self.frame_log:
                stamp = message.header.stamp.sec + message.header.stamp.nanosec * 1e-9
                self.frame_log.write(f"{channel} {zlib.crc32(data)} {len(data)} {stamp:.6f} {received_at:.6f} {published_at:.6f} {marker}\n")

    def mark(self, data, counter):
        """Draws `counter` as black/white blocks into the luma (chroma made neutral) and re-encodes."""
        import numpy as np

        _, _, subsample, _ = self.turbo.decode_header(data)
        buffer, sizes = self.turbo.decode_to_yuv(data, pad=1)
        planes = np.frombuffer(bytearray(buffer), dtype=np.uint8)
        (height, width), (chroma_height, chroma_width) = sizes[0], sizes[1]
        luma = planes[: height * width].reshape(height, width)
        chroma_u = planes[height * width : height * width + chroma_height * chroma_width].reshape(chroma_height, chroma_width)
        chroma_v = planes[height * width + chroma_height * chroma_width : height * width + 2 * chroma_height * chroma_width].reshape(chroma_height, chroma_width)
        block = int(round(width * MARK_BLOCK_FRACTION))
        bits = [1, 0] + [(counter >> (15 - index)) & 1 for index in range(16)]
        for index, bit in enumerate(bits):
            left, right = index * block, (index + 1) * block
            luma[0:block, left:right] = 255 if bit else 0
            chroma_rows = slice(0, block * chroma_height // height)
            chroma_columns = slice(left * chroma_width // width, right * chroma_width // width)
            chroma_u[chroma_rows, chroma_columns] = 128
            chroma_v[chroma_rows, chroma_columns] = 128
        return self.turbo.encode_from_yuv(planes, height, width, quality=MARK_QUALITY, jpeg_subsample=subsample)

    def report(self):
        elapsed = time.time() - self.started
        rates = ", ".join(f"{channel} {count / elapsed:.1f} Hz" for channel, count in self.counts.items())
        print(f"[relay] {rates}", flush=True)


def main():
    rclpy.init(args=sys.argv)
    rclpy.spin(CameraRelay())


if __name__ == "__main__":
    main()
