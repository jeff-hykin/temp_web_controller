#!/usr/bin/env -S deno run --allow-all
// End-to-end: web_ctrl against a fake robot (examples/test_rig.rs), driven from its
// own headless Chrome (never the one on port 9222).
//
//   deno run --allow-all test/e2e.js
//
// Two runs. In the first nothing answers on the --zenoh-web port, so web_ctrl starts
// zenoh-web in-process; in the second a zenoh-web command is already running there
// and web_ctrl must use it rather than start its own. Each run checks:
//   - camera tiles show decoded, non-black frames: an lcm rgb8 image (relayed to zenoh
//     by web_ctrl), an lcm 16UC1 depth image (canvas), a zenoh CompressedImage (png)
//   - the lcm relay is lazy: nothing is copied onto zenoh while no tile watches
//   - a recording started from the page contains the camera topics
//   - steering reaches /tele_cmd_vel_test on zenoh and (mirrored) lcm with REP-103
//     signs, goes quiet after release, and the deadman zero arrives when the page dies
// Everything is isolated: zenoh never scouts, lcm uses its own multicast port, and
// the only command topic is /tele_cmd_vel_test.
//
// Needs a zenoh-web binary for the second run: $ZENOH_WEB_BIN, else one built from
// the rev Cargo.toml pins into target/test-tools (first run takes a few minutes).

import { $ } from "https://esm.sh/dax-sh@0.42.0"
import { launch } from "jsr:@astral/astral@0.5.6"

const repoRoot = $.path(import.meta.url).parentOrThrow().parentOrThrow()
const scratch = $.path(await Deno.makeTempDir({ prefix: "web_ctrl_e2e_" }))
const fixtures = repoRoot.join("test/fixtures")
const COMMAND_TOPIC = "/tele_cmd_vel_test"
const COMMAND_KEY = "dimos/tele_cmd_vel_test/geometry_msgs.Twist"
const COMMAND_CHANNEL = "/tele_cmd_vel_test#geometry_msgs.Twist"
const CAMERAS = {
    lcmColor: "dimos/test_lcm_cam/sensor_msgs.Image",
    lcmDepth: "dimos/test_lcm_depth/sensor_msgs.Image",
    zenohColor: "dimos/test_zenoh_cam/sensor_msgs.CompressedImage",
}

const failures = []
function check(condition, description) {
    console.log(`${condition ? "PASS" : "FAIL"} ${description}`)
    if (!condition) {
        failures.push(description)
    }
}

function freePort() {
    const listener = Deno.listen({ port: 0, hostname: "127.0.0.1" })
    const port = listener.addr.port
    listener.close()
    return port
}

/** Collects a child's output lines (also into a log file) and resolves waiters on matching lines. */
function lineCollector(stream, name) {
    const lines = []
    let waiters = []
    const log = Deno.openSync(scratch.join(`${name}.log`).toString(), { create: true, append: true })
    const encoder = new TextEncoder()
    ;(async () => {
        let buffered = ""
        for await (const chunk of stream.pipeThrough(new TextDecoderStream())) {
            buffered += chunk
            const parts = buffered.split("\n")
            buffered = parts.pop() ?? ""
            for (const line of parts) {
                log.writeSync(encoder.encode(`${line}\n`))
                lines.push({ line, at: performance.now() })
                waiters = waiters.filter((waiter) => {
                    if (waiter.test(line)) {
                        waiter.resolve(line)
                        return false
                    }
                    return true
                })
            }
        }
    })()
    return {
        lines,
        waitFor(test, timeoutMs) {
            const existing = lines.find((entry) => test(entry.line))
            if (existing) {
                return Promise.resolve(existing.line)
            }
            return new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error(`${name}: timed out waiting for a line`)), timeoutMs)
                waiters.push({ test, resolve: (line) => {
                    clearTimeout(timer)
                    resolve(line)
                } })
            })
        },
    }
}

const children = []
function spawn(name, command) {
    const child = command.stdout("piped").stderr("piped").noThrow().spawn()
    children.push(child)
    return { child, stdout: lineCollector(child.stdout(), name), stderr: lineCollector(child.stderr(), `${name}.err`) }
}

async function stop(child) {
    try {
        child.kill("SIGTERM")
    } catch {
        // already gone
    }
    await child.catch(() => {})
}

/** The commands of every process listening on `port`. */
async function listeners(port) {
    const output = await $`lsof -nP -iTCP:${port} -sTCP:LISTEN -t`.noThrow().text()
    const pids = [...new Set(output.split("\n").filter(Boolean))]
    const commands = []
    for (const pid of pids) {
        commands.push((await $`ps -o comm= -p ${pid}`.noThrow().text()).trim().split("/").pop())
    }
    return commands
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

async function until(condition, timeoutMs, intervalMs = 200) {
    const deadline = performance.now() + timeoutMs
    while (performance.now() < deadline) {
        const value = await condition()
        if (value) {
            return value
        }
        await sleep(intervalMs)
    }
    return await condition()
}

async function zenohWebBinary() {
    const fromEnvironment = Deno.env.get("ZENOH_WEB_BIN")
    if (fromEnvironment) {
        return fromEnvironment
    }
    const rev = repoRoot.join("Cargo.toml").readTextSync().match(/zenoh-web = \{[^}]*rev = "([0-9a-f]+)"/)[1]
    const root = repoRoot.join(`target/test-tools/zenoh-web-${rev.slice(0, 12)}`)
    const binary = root.join("bin/zenoh-web")
    if (!binary.existsSync()) {
        $.logStep(`building zenoh-web ${rev.slice(0, 7)} for the already-running case`)
        await $`cargo install --quiet --locked --git https://github.com/jeff-hykin/zenoh-web --rev ${rev} --root ${root} zenoh-web`
    }
    return binary.toString()
}

/** A zenoh config that never scouts: test processes only meet whom they are pointed at. */
function isolatedConfig(name, listen) {
    const path = scratch.join(`${name}.json5`)
    path.writeTextSync(JSON.stringify({ mode: "peer", scouting: { multicast: { enabled: false } }, listen: { endpoints: listen } }))
    return path.toString()
}

$.logStep("building web_ctrl and the test rig (release)")
await $`cargo build --release --quiet --bin web_ctrl --example test_rig`.cwd(repoRoot)
const zenohWeb = await zenohWebBinary()
const webCtrlBinary = repoRoot.join("target/release/web_ctrl").toString()

const lcmUrl = `udpm://239.255.76.67:${20000 + Math.floor(Math.random() * 20000)}?ttl=0`
const rigZenohPort = freePort()
const rig = spawn("rig", $`${repoRoot.join("target/release/examples/test_rig")}
    --zenoh-listen tcp/127.0.0.1:${rigZenohPort} --lcm-url ${lcmUrl}
    --lcm-image ${`/test_lcm_cam#sensor_msgs.Image=${fixtures.join("image_rgb8.bin")}@10`}
    --lcm-image ${`/test_lcm_depth#sensor_msgs.Image=${fixtures.join("depth_16UC1.bin")}@10`}
    --zenoh-image ${`${CAMERAS.zenohColor}=${fixtures.join("compressed_png.bin")}@10`}
    --watch-zenoh ${COMMAND_KEY} --watch-lcm ${COMMAND_CHANNEL}`)
await rig.stdout.waitFor((line) => line === "READY", 15000)

const browser = await launch({ headless: true, args: ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"] })

/** Commands the rig heard since `since`, parsed. */
function commandsSince(since, transport) {
    return rig.stdout.lines
        .filter((entry) => entry.at >= since && entry.line.startsWith(`${transport} `))
        .map((entry) => ({ at: entry.at, values: entry.line.split(" ").slice(2).map(Number) }))
}
const isZero = (command) => command.values.length === 6 && command.values.every((value) => value === 0)

async function scenario(name, { external }) {
    $.logStep(`${name}: zenoh-web ${external ? "already running" : "started by web_ctrl"}`)
    const zenohWebPort = freePort()
    let externalBridge = null
    const connects = ["--zenoh-connect", `tcp/127.0.0.1:${rigZenohPort}`]
    if (external) {
        const bridgeZenohPort = freePort()
        externalBridge = spawn(`${name}.zenoh-web`, $`${zenohWeb} --port ${zenohWebPort}
            --zenoh-config ${isolatedConfig(`${name}.zenoh-web`, [`tcp/127.0.0.1:${bridgeZenohPort}`])}
            --connect tcp/127.0.0.1:${rigZenohPort}`.env("RUST_LOG", "info,zenoh=warn,zenoh_web=info"))
        await externalBridge.stderr.waitFor((line) => line.includes("listening on"), 20000)
        connects.push("--zenoh-connect", `tcp/127.0.0.1:${bridgeZenohPort}`)
    }

    const httpPort = freePort()
    const recordDir = scratch.join(`${name}.recordings`)
    const webCtrl = spawn(`${name}.web_ctrl`, $`${webCtrlBinary} --bind 127.0.0.1 --port ${httpPort}
        --topic ${COMMAND_TOPIC} --lcm-url ${lcmUrl}
        --zenoh-web ${`http://127.0.0.1:${zenohWebPort}`}
        --zenoh-config ${isolatedConfig(`${name}.web_ctrl`, ["tcp/127.0.0.1:0"])} ${connects}
        --record-dir ${recordDir} --launch-file ${scratch.join(`${name}.launch.json`)}`)
    const modeLine = await webCtrl.stdout.waitFor((line) => line.includes("zenoh-web ->"), 30000)
    const base = `http://127.0.0.1:${httpPort}`
    const status = await (await fetch(`${base}/api/status`)).json()
    const expectedMode = external ? "external" : "in-process"
    check(status.zenoh_web.mode === expectedMode, `${name}: web_ctrl reports zenoh-web ${expectedMode} (${modeLine.trim()})`)
    const holders = await listeners(zenohWebPort)
    const expectedHolder = external ? "zenoh-web" : "web_ctrl"
    check(holders.length === 1 && holders[0] === expectedHolder,
        `${name}: port ${zenohWebPort} is held only by ${external ? "the zenoh-web that was already running" : "web_ctrl's in-process zenoh-web"} (${holders})`)
    const health = await fetch(`http://127.0.0.1:${zenohWebPort}/zenoh-web/health`).then((response) => response.json()).catch(() => null)
    check(health?.service === "zenoh-web", `${name}: GET /zenoh-web/health answers on the zenoh-web port`)

    // Nothing watches yet, so the relay must not have copied a frame onto zenoh.
    // A plain zenoh subscriber would itself be a watcher, so this asks web_ctrl's
    // lcm side instead: an unwatched fragmented image is never even reassembled.
    await sleep(1500)
    const idle = (await (await fetch(`${base}/api/status`)).json()).topics
    const idleDepth = idle.find((topic) => topic.topic === "test_lcm_depth")
    check(idleDepth && idleDepth.rate > 0 && idleDepth.encoding === null,
        `${name}: an unwatched lcm camera is discovered but never reassembled (rate ${idleDepth?.rate?.toFixed(1)}, encoding ${idleDepth?.encoding})`)

    const page = await browser.newPage(`${base}/`)
    const consoleErrors = []
    page.addEventListener("console", (event) => {
        if (event.detail.type === "error") {
            consoleErrors.push(event.detail.text)
        }
    })

    const chips = await until(() => page.evaluate((wanted) => {
        const keys = [...document.querySelectorAll("#camera-picker .chip")].map((chip) => chip.dataset.key)
        return wanted.every((key) => keys.includes(key)) ? keys : null
    }, { args: [Object.values(CAMERAS)] }), 20000)
    check(Boolean(chips), `${name}: the camera picker lists the lcm and zenoh cameras (${chips})`)

    await page.evaluate((wanted) => {
        for (const key of wanted) {
            const chip = document.querySelector(`#camera-picker .chip[data-key="${CSS.escape(key)}"]`)
            if (chip && !chip.classList.contains("on")) {
                chip.click()
            }
        }
    }, { args: [Object.values(CAMERAS)] })

    // Each tile's picture, as the page shows it: the <video> drawn onto a canvas, or
    // the depth canvas itself. Quadrants are the fixture's red / green / blue / white.
    const sampleTiles = () => page.evaluate(() => {
        const out = {}
        for (const tile of document.querySelectorAll("#streams .tile")) {
            const media = tile.querySelector("video, canvas")
            const width = media.videoWidth ?? media.width
            const height = media.videoHeight ?? media.height
            if (!width || !height) {
                out[tile.dataset.key] = null
                continue
            }
            const canvas = document.createElement("canvas")
            canvas.width = width
            canvas.height = height
            const context = canvas.getContext("2d")
            context.drawImage(media, 0, 0)
            const pixels = context.getImageData(0, 0, width, height).data
            const at = (fractionX, fractionY) => {
                const index = (Math.floor(fractionY * height) * width + Math.floor(fractionX * width)) * 4
                return [pixels[index], pixels[index + 1], pixels[index + 2]]
            }
            let lit = 0
            for (let index = 0; index < pixels.length; index += 4) {
                lit += pixels[index] + pixels[index + 1] + pixels[index + 2] > 60 ? 1 : 0
            }
            out[tile.dataset.key] = {
                kind: media.tagName.toLowerCase(),
                width,
                height,
                litFraction: lit / (pixels.length / 4),
                topLeft: at(0.25, 0.25),
                topRight: at(0.75, 0.25),
                bottomLeft: at(0.25, 0.75),
                info: tile.querySelector(".tile-bar span").textContent,
            }
        }
        return out
    })
    const redGreenBlue = (sample) => sample && sample.topLeft[0] > 180 && sample.topLeft[1] < 90 && sample.topRight[1] > 180 && sample.topRight[0] < 90 && sample.bottomLeft[2] > 180
    const samples = await until(async () => {
        const sampled = await sampleTiles()
        const ready = redGreenBlue(sampled[CAMERAS.lcmColor]) && redGreenBlue(sampled[CAMERAS.zenohColor]) && sampled[CAMERAS.lcmDepth]?.litFraction > 0.9
        return ready ? sampled : null
    }, 30000, 500) ?? await sampleTiles()
    console.log(JSON.stringify(samples))
    check(samples[CAMERAS.lcmColor]?.kind === "video" && redGreenBlue(samples[CAMERAS.lcmColor]),
        `${name}: the lcm rgb8 camera shows as video with the fixture's colours (${JSON.stringify(samples[CAMERAS.lcmColor])})`)
    check(samples[CAMERAS.zenohColor]?.kind === "video" && redGreenBlue(samples[CAMERAS.zenohColor]),
        `${name}: the zenoh CompressedImage camera shows as video with the fixture's colours (${JSON.stringify(samples[CAMERAS.zenohColor])})`)
    check(samples[CAMERAS.lcmDepth]?.kind === "canvas" && samples[CAMERAS.lcmDepth].litFraction > 0.9 && samples[CAMERAS.lcmDepth].width === 320,
        `${name}: the lcm 16UC1 camera is drawn as depth, every pixel coloured (${JSON.stringify(samples[CAMERAS.lcmDepth])})`)
    const watched = (await (await fetch(`${base}/api/status`)).json()).topics.find((topic) => topic.topic === "test_lcm_depth")
    check(watched?.encoding === "16UC1", `${name}: once watched, the lcm depth camera is reassembled (encoding ${watched?.encoding})`)

    $.logStep(`${name}: recording`)
    await page.evaluate(() => {
        document.getElementById("record-open").click()
        document.getElementById("record-toggle").click()
    })
    const recording = await until(async () => {
        const live = (await (await fetch(`${base}/api/status`)).json()).recording
        return live.active && live.messages > 30 ? live : null
    }, 15000)
    check(Boolean(recording), `${name}: the record button starts a recording (${recording?.messages} messages so far)`)
    await page.evaluate(() => document.getElementById("record-toggle").click())
    const files = await until(async () => {
        const listed = await (await fetch(`${base}/api/recordings`)).json()
        const live = (await (await fetch(`${base}/api/status`)).json()).recording
        return !live.active && listed.length > 0 ? listed : null
    }, 15000)
    const mcap = files ? Deno.readFileSync(files[0].path) : new Uint8Array()
    const text = new TextDecoder().decode(mcap)
    const magic = [0x89, 0x4d, 0x43, 0x41, 0x50, 0x30, 0x0d, 0x0a]
    check(magic.every((byte, index) => mcap[index] === byte) && text.includes("test_lcm_cam") && text.includes("test_lcm_depth") && text.includes("dimos/test_zenoh_cam"),
        `${name}: the mcap (${files?.[0]?.name}, ${mcap.length} bytes) contains the lcm and zenoh camera topics`)

    $.logStep(`${name}: steering on ${COMMAND_TOPIC}`)
    const pressedAt = performance.now()
    await page.evaluate(() => {
        document.getElementById("record-close").click()
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "w", bubbles: true }))
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "d", bubbles: true }))
    })
    const driving = await until(() => {
        const zenohCommands = commandsSince(pressedAt, "ZENOH").filter((command) => !isZero(command))
        const lcmCommands = commandsSince(pressedAt, "LCM").filter((command) => !isZero(command))
        return zenohCommands.length >= 5 && lcmCommands.length >= 5 ? { zenohCommands, lcmCommands } : null
    }, 10000)
    const sample = driving?.zenohCommands.at(-1)?.values
    check(Boolean(driving), `${name}: holding W+D publishes on zenoh and is mirrored onto lcm (${driving?.zenohCommands.length} zenoh, ${driving?.lcmCommands.length} lcm)`)
    check(sample && Math.abs(sample[0] - 0.25) < 1e-9 && Math.abs(sample[5] + 0.5) < 1e-9 && sample[1] === 0,
        `${name}: W+D is linear.x = +0.25, angular.z = -0.5 (REP-103: right turns clockwise) (${sample})`)

    const releasedAt = performance.now()
    await page.evaluate(() => {
        document.body.dispatchEvent(new KeyboardEvent("keyup", { key: "w", bubbles: true }))
        document.body.dispatchEvent(new KeyboardEvent("keyup", { key: "d", bubbles: true }))
    })
    await sleep(3000)
    const afterRelease = commandsSince(releasedAt, "ZENOH")
    const lastNonZero = afterRelease.filter((command) => !isZero(command)).at(-1)
    const zerosAfter = afterRelease.filter((command) => isZero(command) && (!lastNonZero || command.at > lastNonZero.at))
    const quietSince = performance.now() - 1500
    check(zerosAfter.length >= 5 && afterRelease.filter((command) => command.at >= quietSince).length === 0,
        `${name}: a release sends a second of zeros, then the topic goes quiet (${zerosAfter.length} zeros, ${afterRelease.filter((command) => command.at >= quietSince).length} in the last 1.5 s)`)

    $.logStep(`${name}: deadman`)
    await page.evaluate(() => document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "w", bubbles: true })))
    await until(() => commandsSince(performance.now() - 300, "ZENOH").some((command) => !isZero(command)), 5000)
    await sleep(500)
    const closedAt = performance.now()
    await page.close()
    const deadman = await until(() => {
        const zenohZero = commandsSince(closedAt, "ZENOH").find(isZero)
        const lcmZero = commandsSince(closedAt, "LCM").find(isZero)
        return zenohZero && lcmZero ? { zenohZero, lcmZero } : null
    }, 5000)
    const afterClose = commandsSince(closedAt, "ZENOH")
    check(Boolean(deadman) && isZero(afterClose.at(-1)),
        `${name}: closing the page mid-drive fires the deadman: a zero twist on zenoh after ${deadman ? (deadman.zenohZero.at - closedAt).toFixed(0) : "?"} ms and on lcm, and nothing after it`)

    check(consoleErrors.length === 0, `${name}: no console errors (${consoleErrors.join(" | ")})`)

    await stop(webCtrl.child)
    if (externalBridge) {
        await stop(externalBridge.child)
    }
}

try {
    await scenario("in-process", { external: false })
    await scenario("external", { external: true })
} catch (error) {
    failures.push(`crashed: ${error.stack ?? error}`)
    console.log(`FAIL crashed: ${error.stack ?? error}`)
} finally {
    await browser.close().catch(() => {})
    for (const child of children) {
        await stop(child)
    }
}
console.log(`\n${failures.length === 0 ? "ALL PASSED" : `${failures.length} FAILED:\n  ${failures.join("\n  ")}`}`)
console.log(`logs: ${scratch}`)
Deno.exit(failures.length === 0 ? 0 : 1)
