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
                                  (also editable in the record panel)
--launch-file <PATH>              saved launcher commands [default: ~/.dimos/temp_web_control.json]
```

## Start on boot

```sh
web_ctrl survive_reboot [same options you would normally pass]
```

Installs a systemd unit on Linux or a launchd daemon on macOS, then starts it,
so the robot comes back on its own after a power cut. It asks for sudo. The
options you pass are baked into the service, and relative paths are made
absolute first since a service does not inherit your shell's directory.

If `web_ctrl` came from nix, the service points at `~/.nix-profile/bin/web_ctrl`
rather than the `/nix/store` path, so a later `nix profile upgrade` reaches it.

Undo it with `sudo systemctl disable --now web_ctrl`, or on macOS
`sudo launchctl bootout system/com.jeffhykin.web_ctrl`.

## Controls

- `W`/`A`/`S`/`D` or arrow keys to drive, `Q`/`E` to strafe, space to stop
- drag the on-screen stick on touch devices
- the button pad next to the stick holds one axis at exactly full scale, which is
  what you want for driving perfectly straight during a recording; the top row
  strafes left, drives forward, and strafes right
- tap a camera chip to open or close that stream
- the Record button turns red and shows the file size while a recording is running
- the settings drawer adjusts speeds, deadman timeout, and image quality
- the command topic can be renamed there while running, for robots that do not use
  `/tele_cmd_vel`; a name that cannot be used leaves the current one in place

Commands expire after the deadman timeout (400 ms default), so a dropped connection stops the
robot rather than latching the last command.

Nothing is published while nobody is steering. The topic only carries traffic while a control
is held, followed by a second of zeros so the stop is heard, and then goes quiet — otherwise a
parked browser would drown out every other teleop source on the same topic.

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

The Record button starts and stops an mcap file, written by a background thread in the
backend so the video path never waits on the disk. Every topic on the wire is recorded by
default, including ones that appear mid-recording; unchecking a topic is what leaves it out.
Unchecked topics are remembered in the browser's local storage and re-applied when the server
restarts, so the same handful does not have to be unchecked every session. Only the exclusions
are stored, so a topic seen for the first time is still recorded.

Types the binary understands (`Image`, `CompressedImage`, `Twist`, `TFMessage`, `PointCloud2`,
`Odometry`, `PoseStamped`, `Imu`, `CameraInfo`) are transcoded to ROS2 CDR with a schema, so the
file opens in Foxglove directly. An `Image` that actually carries a jpeg or png stream — which
is what dimos's `JpegLcmTransport` sends — is recorded as a `CompressedImage` instead, since an
`Image` whose `encoding` names a codec is not something Foxglove's image panel will draw.
Anything else is stored as raw LCM bytes under a schema-less
channel tagged with its type name, so a recording is never silently incomplete.

The panel also lists finished files with their size and age, a button that copies the absolute
path, and a delete that needs a second click to confirm.

## Launcher

The Launch button opens a panel that runs shell commands on the machine hosting the binary.
A command is a name paired with a bash line; saving one adds a row with its own Launch button.
The last 40 stdout and stderr lines stream into the panel while it runs, and the exit code is
shown in red if it crashes. Copy lifts that output to the clipboard. The pane only follows the
tail while you are parked at the bottom, so scrolling up to read does not get yanked back down.

Commands live on the robot, in `~/.dimos/temp_web_control.json`, not in the browser — one
person can add a command and someone else can run it from their own phone. The output stream
and running state are shared the same way, so everyone watching sees the same thing.

Only one command runs at a time. Each gets its own process group, so Stop takes down the whole
tree rather than just the shell that spawned it.

Kill blueprint is the `kd` script inlined: it kills every dimos process it can find (each clone's
venv, native modules under `result/bin` and `rust/target/release`, viewers, simulators, ROS),
frees the ports dimos uses, and removes dimos docker containers. It deliberately skips `web_ctrl`
and `claude` so it cannot kill the server serving the page.

**This is an unauthenticated remote shell.** The server binds `0.0.0.0` with no auth, so anyone
who can reach the port can run commands as the user running the binary. Use `--bind 127.0.0.1`
or a trusted network.

## Develop

```sh
nix develop
cargo test
cargo clippy --all-targets
```
