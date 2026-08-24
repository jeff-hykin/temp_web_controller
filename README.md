# web_ctrl

A single self-contained binary that serves a phone/desktop drive controller for dimos robots.
It listens on both LCM and zenoh, discovers every topic on the wire, streams any image topic
(rgb, grayscale, depth, compressed) into the browser as JPEG, and publishes `/tele_cmd_vel`
back out on both transports.

Image delivery is real-time first: frames are dropped and quality is degraded automatically
rather than allowing the view to fall behind.

## Install

```sh
nix profile install github:jeff-hykin/temp_web_controller
```

That puts `web_ctrl` on your PATH. To run it once without installing, use
`nix run github:jeff-hykin/temp_web_controller` instead.

Then open the URL it prints (`http://<lan ip>:8099`) from a phone or laptop on the same network.

LCM uses `ttl=0` multicast, so this must run **on** the robot to see its traffic.

## Options

```
--port <PORT>                     http port [default: 8099]
--bind <ADDR>                     bind address [default: 0.0.0.0]
--topic <TOPIC>                   command topic [default: /tele_cmd_vel]
--transport <both|lcm|zenoh>      where to publish commands [default: both]
--lcm-url <URL>                   [default: udpm://239.255.76.67:7667?ttl=0]
--linear-speed <M_PER_S>          [default: 0.25]
--angular-speed <RAD_PER_S>       [default: 0.5]
--record-dir <DIR>                where mcap recordings are written [default: recordings]
```

## Controls

- `W`/`A`/`S`/`D` or arrow keys to drive, `Q`/`E` to strafe, space to stop
- drag the on-screen stick on touch devices
- the button pad next to the stick holds one axis at exactly full scale, which is
  what you want for driving perfectly straight during a recording; the top row
  strafes left, drives forward, and strafes right
- tap a camera chip to open or close that stream
- the settings drawer adjusts speeds, deadman timeout, and image quality

Commands expire after the deadman timeout (400 ms default), so a dropped connection stops the
robot rather than latching the last command.

## Transform tree

`tf2_msgs.TFMessage` is decoded off both transports and drawn as a graph under
Settings → Transform tree. A red dot appears on the Settings button whenever
something is wrong:

- a frame with two parents
- a disjoint forest (more than one root)
- a cycle
- an edge that stopped arriving, which `/tf_static` is exempt from since it publishes rarely

It refreshes every two seconds, which is deliberate — tf is a diagnostic here,
not part of the video path.

## Recording

Settings → Recording starts and stops an mcap file, written by a background thread in the
backend so the video path never waits on the disk. Every topic on the wire is recorded by
default, including ones that appear mid-recording; unchecking a topic is what leaves it out.

Types the binary understands (`Image`, `CompressedImage`, `Twist`, `TFMessage`) are transcoded
to ROS2 CDR with a schema, so the file opens in Foxglove directly. Anything else is stored as
raw LCM bytes under a schema-less channel tagged with its type name, so a recording is never
silently incomplete.

The panel also lists finished files with their size and age, a button that copies the absolute
path, and a delete that needs a second click to confirm.

## Develop

```sh
nix develop
cargo test
cargo clippy --all-targets
```
