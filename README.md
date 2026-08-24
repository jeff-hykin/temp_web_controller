# web_ctrl

A single self-contained binary that serves a phone/desktop drive controller for dimos robots.
It listens on both LCM and zenoh, discovers every topic on the wire, streams any image topic
(rgb, grayscale, depth, compressed) into the browser as JPEG, and publishes `/tele_cmd_vel`
back out on both transports.

Image delivery is real-time first: frames are dropped and quality is degraded automatically
rather than allowing the view to fall behind.

## Run

```sh
nix run github:jeff-hykin/temp_web_controller
# or
cargo run --release
```

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
```

## Controls

- `W`/`A`/`S`/`D` or arrow keys to drive, `Q`/`E` to strafe, space to stop
- drag the on-screen stick on touch devices
- tap a camera chip to open or close that stream
- the settings drawer adjusts speeds, publish rate, deadman timeout, and image quality

Commands expire after the deadman timeout (400 ms default), so a dropped connection stops the
robot rather than latching the last command.

## Develop

```sh
nix develop
cargo test
cargo clippy --all-targets
```
