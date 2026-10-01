#!/usr/bin/env -S deno run --allow-all
// Camera streaming on a real robot, measured from this machine's own headless Chrome
// (never the one on port 9222): frames/s the browser actually shows, bytes per frame,
// throughput, end-to-end latency, and the web_ctrl process's CPU on the robot.
//
//   deno run -A test/r1/measure.js --version main --url http://100.68.53.40:18099 --label main-1
//   deno run -A test/r1/measure.js --version branch --url http://100.68.53.40:18100 --label branch-2tabs --second-tab
//
// Needs test/r1/camera_relay.py running on the robot with RELAY_FRAME_LOG set: it logs
// every frame's crc32, camera stamp and publish time, which is the source side of the
// latency. The page is only observed: hooks wrap WebSocket/Blob/createImageBitmap/
// drawImage (main), RTCPeerConnection/requestVideoFrameCallback (branch) to timestamp
// frames, and the harness only clicks camera chips. It never sends a key or touches
// the joystick.
//
// Latency = when the browser shows a frame (Mac clock) - when the relay published it
// (robot clock), with the clock offset from best-of-N ssh round trips (error <= RTT/2).
//   main:   frame identity is exact (crc32 of the jpeg the websocket delivered); shown
//           = the requestAnimationFrame after drawImage.
//   branch: H.264 drops the stamp, so each composited frame (rVFC receiveTime) is paired
//           with the zenoh-web metadata message that arrived closest to it (sent right
//           after the frame is written to the track), whose sample timestamp is then
//           paired with the relay's last publish at or before it; shown = rVFC
//           expectedDisplayTime.

import { parseArgs } from "jsr:@std/cli@1.0.6/parse-args"
import { launch } from "jsr:@astral/astral@0.5.6"

const args = parseArgs(Deno.args, {
    string: ["version", "url", "label", "ssh", "frame-log", "out", "camera", "second-camera"],
    boolean: ["second-tab"],
    default: {
        ssh: "nvidia@100.68.53.40",
        "frame-log": "/tmp/wctest/frames.log",
        seconds: 60,
        warmup: 6,
        camera: "r1_head_left",
        "second-camera": "r1_head_right",
        out: new URL("./results", import.meta.url).pathname,
    },
})
if (!["main", "branch"].includes(args.version) || !args.url || !args.label) {
    console.error("usage: measure.js --version main|branch --url http://host:port --label name [--second-tab] [--seconds 60]")
    Deno.exit(2)
}
const seconds = Number(args.seconds)
const processName = `web_ctrl_${args.version}`

const decoder = new TextDecoder()
async function ssh(command) {
    const output = await new Deno.Command("ssh", { args: ["-o", "ConnectTimeout=10", args.ssh, command], stdout: "piped", stderr: "piped" }).output()
    if (!output.success) {
        throw new Error(`ssh ${command}: ${decoder.decode(output.stderr)}`)
    }
    return decoder.decode(output.stdout)
}

const macNowMs = () => performance.timeOrigin + performance.now()

/** robot clock - Mac clock, from the lowest-RTT of `count` round trips over one ssh session. */
async function clockOffset(count = 80) {
    const child = new Deno.Command("ssh", {
        args: [args.ssh, "python3 -u -c 'import sys,time\nfor line in sys.stdin: print(repr(time.time()), flush=True)'"],
        stdin: "piped", stdout: "piped", stderr: "null",
    }).spawn()
    const writer = child.stdin.getWriter()
    const lines = child.stdout.pipeThrough(new TextDecoderStream()).getReader()
    let buffered = ""
    const readLine = async () => {
        while (!buffered.includes("\n")) {
            const { value, done } = await lines.read()
            if (done) {
                throw new Error("clock ssh closed")
            }
            buffered += value
        }
        const index = buffered.indexOf("\n")
        const line = buffered.slice(0, index)
        buffered = buffered.slice(index + 1)
        return line
    }
    const samples = []
    for (let index = 0; index < count + 3; index++) {
        const sent = macNowMs()
        await writer.write(new TextEncoder().encode("x\n"))
        const robotMs = Number(await readLine()) * 1000
        const received = macNowMs()
        if (index >= 3) {
            samples.push({ rttMs: received - sent, offsetMs: robotMs - (sent + received) / 2 })
        }
        await new Promise((resolve) => setTimeout(resolve, 20))
    }
    await writer.close()
    await child.status
    samples.sort((a, b) => a.rttMs - b.rttMs)
    const best = samples[0]
    const fewBest = samples.slice(0, 5).map((sample) => sample.offsetMs)
    return { offsetMs: best.offsetMs, errorMs: best.rttMs / 2, rttMinMs: best.rttMs, rttMedianMs: samples[Math.floor(samples.length / 2)].rttMs, spreadOfBest5Ms: Math.max(...fewBest) - Math.min(...fewBest) }
}

/** %CPU (100 = one core) of the web_ctrl process, once a second, while the window runs. */
function sampleCpu(count) {
    return (async () => {
        const text = await ssh(`pid=$(pgrep -x ${processName} | head -1); [ -n "$pid" ] && top -b -d 1 -n ${count + 1} -p $pid`)
        const values = text.split("\n").map((line) => line.trim().split(/\s+/)).filter((fields) => /^\d+$/.test(fields[0]) && fields.length >= 12).map((fields) => Number(fields[8]))
        return values.slice(1) // top's first sample is since process start
    })()
}

// Runs in the page before its own scripts. Observes only; every wrapper calls through.
const HOOKS = `(() => {
    const M = window.__measure = { ws: [], draws: [], meta: [], vfc: [], pcs: [], bridgeStats: [] }
    const table = new Uint32Array(256)
    for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) { c = c & 1 ? 0xEDB88320 ^ (c >>> 1) : c >>> 1 } table[n] = c >>> 0 }
    const crc32 = (bytes) => { let c = 0xFFFFFFFF; for (let i = 0; i < bytes.length; i++) { c = table[(c ^ bytes[i]) & 0xFF] ^ (c >>> 8) } return (c ^ 0xFFFFFFFF) >>> 0 }
    const crcOf = new WeakMap()
    const markerOf = new WeakMap()
    // The relay's frame counter (camera_relay.py RELAY_MARK): 18 blocks of 5% of the width along the top,
    // white and black guards then 16 bits. Read from our own small canvas, never the page's.
    const BLOCKS = 18, BLOCK = 0.05
    const probe = new OffscreenCanvas(BLOCKS * 4, 1).getContext("2d", { willReadFrequently: true })
    const readMarker = (source, width, height) => {
        if (!width || !height) { return null }
        const side = width * BLOCK
        probe.drawImage(source, 0, side * 0.3, side * BLOCKS, side * 0.4, 0, 0, BLOCKS * 4, 1)
        const pixels = probe.getImageData(0, 0, BLOCKS * 4, 1).data
        const luma = (index) => { const at = (index * 4 + 1) * 4, at2 = (index * 4 + 2) * 4; return (pixels[at] + pixels[at + 1] + pixels[at + 2] + pixels[at2] + pixels[at2 + 1] + pixels[at2 + 2]) / 6 }
        const white = luma(0), black = luma(1)
        if (white - black < 80) { return null }
        const middle = (white + black) / 2
        let value = 0
        for (let bit = 0; bit < 16; bit++) {
            const level = luma(2 + bit)
            if (Math.abs(level - middle) < (white - black) * 0.2) { return null }
            value = (value << 1) | (level > middle ? 1 : 0)
        }
        return value
    }

    const NativeWebSocket = window.WebSocket
    window.WebSocket = class extends NativeWebSocket {
        constructor(url, protocols) {
            super(url, protocols)
            const stream = String(url).match(/\\/ws\\/stream\\/(.*)$/)
            if (stream) {
                this.addEventListener("message", (event) => {
                    if (typeof event.data === "string") { return }
                    const at = performance.now()
                    const crc = crc32(new Uint8Array(event.data))
                    crcOf.set(event.data, crc)
                    M.ws.push({ at, topic: decodeURIComponent(stream[1]), bytes: event.data.byteLength, crc, marker: null })
                })
            }
        }
    }
    const NativeBlob = window.Blob
    window.Blob = class extends NativeBlob {
        constructor(parts, options) {
            super(parts, options)
            if (parts && parts.length === 1 && crcOf.has(parts[0])) { crcOf.set(this, crcOf.get(parts[0])) }
        }
    }
    const nativeCreateImageBitmap = window.createImageBitmap
    window.createImageBitmap = async function (source, ...rest) {
        const bitmap = await nativeCreateImageBitmap.call(this, source, ...rest)
        if (crcOf.has(source)) {
            const crc = crcOf.get(source)
            crcOf.set(bitmap, crc)
            const marker = readMarker(bitmap, bitmap.width, bitmap.height)
            markerOf.set(bitmap, marker)
            for (let index = M.ws.length - 1; index >= 0 && index >= M.ws.length - 50; index--) {
                if (M.ws[index].crc === crc) { M.ws[index].marker = marker; break }
            }
        }
        return bitmap
    }
    const nativeDrawImage = CanvasRenderingContext2D.prototype.drawImage
    CanvasRenderingContext2D.prototype.drawImage = function (image, ...rest) {
        const result = nativeDrawImage.call(this, image, ...rest)
        if (crcOf.has(image)) {
            const entry = { crc: crcOf.get(image), marker: markerOf.get(image) ?? null, drawAt: performance.now(), shownAt: null }
            M.draws.push(entry)
            requestAnimationFrame(() => { entry.shownAt = performance.now() })
        }
        return result
    }

    const NativePeerConnection = window.RTCPeerConnection
    window.RTCPeerConnection = class extends NativePeerConnection {
        constructor(...rest) { super(...rest); M.pcs.push(this) }
        createDataChannel(label, init) {
            const channel = super.createDataChannel(label, init)
            let parsed = null
            try { parsed = JSON.parse(label) } catch {}
            if (label === "control") {
                // the bridge's own stats replies (the page polls them), kept for diagnosis
                channel.addEventListener("message", (event) => {
                    if (typeof event.data === "string" && event.data.includes('"channels"')) {
                        M.bridgeStats.push({ at: performance.now(), text: event.data })
                        if (M.bridgeStats.length > 400) { M.bridgeStats.shift() }
                    }
                })
            }
            if (parsed && parsed.type === "sub") {
                channel.addEventListener("message", (event) => {
                    if (!(event.data instanceof ArrayBuffer)) { return }
                    const at = performance.now()
                    const bytes = new Uint8Array(event.data)
                    const view = new DataView(event.data)
                    const keyLength = view.getUint16(0, true)
                    let offset = 2 + keyLength
                    const timestamp = view.getFloat64(offset, true)
                    const seq = view.getUint32(offset + 8, true)
                    const chunkCount = view.getUint32(offset + 20, true)
                    const chunk = bytes.subarray(offset + 24)
                    if (chunkCount !== 1 || chunk.length !== 28 || chunk[0] !== 1) { return }
                    const meta = new DataView(chunk.buffer, chunk.byteOffset, 28)
                    M.meta.push({ at, key: parsed.key, timestamp, seq, keyframe: (chunk[1] & 1) === 1, width: meta.getUint32(4, true), height: meta.getUint32(8, true), quality: meta.getFloat32(20, true), encodedBytes: meta.getUint32(24, true) })
                })
            }
            return channel
        }
    }
    const nativeVfc = HTMLVideoElement.prototype.requestVideoFrameCallback
    if (nativeVfc) {
        HTMLVideoElement.prototype.requestVideoFrameCallback = function (callback) {
            const video = this
            return nativeVfc.call(this, (now, metadata) => {
                M.vfc.push({ at: now, marker: readMarker(video, video.videoWidth, video.videoHeight), key: video.closest(".tile")?.dataset.key ?? null, expectedDisplayTime: metadata.expectedDisplayTime, presentationTime: metadata.presentationTime, receiveTime: metadata.receiveTime ?? null, rtpTimestamp: metadata.rtpTimestamp ?? null, presentedFrames: metadata.presentedFrames, width: metadata.width, height: metadata.height })
                callback(now, metadata)
            })
        }
    }
    M.stats = async () => {
        const out = { video: [], dataBytes: 0, transportBytes: 0, rttMs: null }
        for (const pc of M.pcs) {
            if (pc.connectionState === "closed") { continue }
            const report = await pc.getStats()
            for (const stat of report.values()) {
                if (stat.type === "inbound-rtp" && stat.kind === "video") {
                    out.video.push({ id: stat.id, mid: stat.mid, bytesReceived: stat.bytesReceived, headerBytesReceived: stat.headerBytesReceived, framesReceived: stat.framesReceived, framesDecoded: stat.framesDecoded, framesDropped: stat.framesDropped, keyFramesDecoded: stat.keyFramesDecoded, packetsLost: stat.packetsLost, freezeCount: stat.freezeCount, jitterBufferDelay: stat.jitterBufferDelay, jitterBufferEmittedCount: stat.jitterBufferEmittedCount, totalDecodeTime: stat.totalDecodeTime, frameWidth: stat.frameWidth, frameHeight: stat.frameHeight, decoderImplementation: stat.decoderImplementation })
                } else if (stat.type === "data-channel") {
                    out.dataBytes += stat.bytesReceived ?? 0
                } else if (stat.type === "transport") {
                    out.transportBytes += stat.bytesReceived ?? 0
                } else if (stat.type === "candidate-pair" && stat.nominated && stat.currentRoundTripTime !== undefined) {
                    out.rttMs = stat.currentRoundTripTime * 1000
                }
            }
        }
        return out
    }
})()`

/** Leaves exactly `wanted` (camera name substrings) on, adding before removing so the page never sees zero tiles. */
async function selectCameras(page, wanted) {
    const deadline = Date.now() + 30000
    while (Date.now() < deadline) {
        const ready = await page.evaluate((wanted) => {
            const chips = [...document.querySelectorAll("#camera-picker .chip")]
            return wanted.every((name) => chips.some((chip) => chip.textContent.includes(name))) && chips.some((chip) => chip.classList.contains("on"))
        }, { args: [wanted] })
        if (ready) {
            break
        }
        await new Promise((resolve) => setTimeout(resolve, 300))
    }
    for (const pass of ["add", "remove"]) {
        await page.evaluate((wanted, pass) => {
            for (const chip of document.querySelectorAll("#camera-picker .chip")) {
                const want = wanted.some((name) => chip.textContent.includes(`${name} `) || chip.textContent.includes(`${name}/`))
                const on = chip.classList.contains("on")
                if ((pass === "add" && want && !on) || (pass === "remove" && !want && on)) {
                    chip.click()
                }
            }
        }, { args: [wanted, pass] })
        await new Promise((resolve) => setTimeout(resolve, 500))
    }
    return await page.evaluate(() => [...document.querySelectorAll("#camera-picker .chip.on")].map((chip) => chip.textContent))
}

const quantile = (values, q) => {
    if (!values.length) {
        return null
    }
    const sorted = [...values].sort((a, b) => a - b)
    return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))]
}
const round = (value, digits) => Math.round(value * 10 ** digits) / 10 ** digits
const mean = (values) => (values.length ? values.reduce((a, b) => a + b, 0) / values.length : null)
const summarize = (values) => ({ n: values.length, p50: quantile(values, 0.5), p95: quantile(values, 0.95), mean: mean(values), min: values.length ? Math.min(...values) : null })

function channelFor(camera) {
    return `/${camera}#sensor_msgs.Image`
}

/** Per-tab, per-camera numbers from the page's hooks and the relay's frame log. */
function analyze({ data, version, camera, relay, offset }) {
    const windowMs = data.end - data.start
    const perSecond = (count) => count / (windowMs / 1000)
    const inWindow = (at) => at >= data.start && at <= data.end
    const toMac = (pageMs) => data.timeOrigin + pageMs
    const channel = channelFor(camera)
    const published = relay.filter((frame) => frame.channel === channel)
    const sourceMac = (frame) => frame.publishedMs - offset.offsetMs
    const cameraMac = (frame) => frame.stampMs - offset.offsetMs
    const relayInWindow = published.filter((frame) => sourceMac(frame) >= toMac(data.start) && sourceMac(frame) <= toMac(data.end))
    // the counter wraps at 65536 frames (~36 min), so take the publish of that marker nearest in time
    const byMarker = new Map()
    for (const frame of published) {
        if (frame.marker >= 0) {
            byMarker.set(frame.marker, [...(byMarker.get(frame.marker) ?? []), frame])
        }
    }
    const sourceOf = (marker, macMs) => {
        const candidates = marker === null ? null : byMarker.get(marker)
        if (!candidates) {
            return null
        }
        return candidates.reduce((best, frame) => (Math.abs(macMs - sourceMac(frame)) < Math.abs(macMs - sourceMac(best)) ? frame : best))
    }
    const common = {
        camera,
        sourceFps: perSecond(relayInWindow.length),
        sourceBytesPerFrame: mean(relayInWindow.map((frame) => frame.bytes)),
        relayReceiveToPublishMs: summarize(relayInWindow.map((frame) => frame.publishedMs - frame.receivedMs)),
        cameraStampToRelayReceiveMs: summarize(relayInWindow.map((frame) => frame.receivedMs - frame.stampMs)),
    }
    if (version === "main") {
        const received = data.ws.filter((frame) => inWindow(frame.at) && frame.topic.includes(camera))
        const draws = data.draws.filter((draw) => inWindow(draw.drawAt))
        const shownPairs = draws.filter((draw) => draw.shownAt !== null).map((draw) => ({ draw, source: sourceOf(draw.marker, toMac(draw.shownAt)) })).filter((pair) => pair.source)
        const arrivalPairs = received.map((frame) => ({ frame, source: sourceOf(frame.marker, toMac(frame.at)) })).filter((pair) => pair.source)
        const bytes = received.reduce((sum, frame) => sum + frame.bytes, 0)
        const markers = new Set(draws.map((draw) => draw.marker).filter((marker) => marker !== null))
        return {
            ...common,
            shownFps: perSecond(draws.length),
            receivedFps: perSecond(received.length),
            distinctFramesShownFps: perSecond(markers.size),
            markerReadFraction: draws.filter((draw) => draw.marker !== null).length / Math.max(1, draws.length),
            matchedFraction: shownPairs.length / Math.max(1, draws.length),
            bytesPerFrame: received.length ? bytes / received.length : null,
            mbps: (bytes * 8) / (windowMs / 1000) / 1e6,
            latencyShownMs: summarize(shownPairs.map(({ draw, source }) => toMac(draw.shownAt) - sourceMac(source))),
            latencyArrivalMs: summarize(arrivalPairs.map(({ frame, source }) => toMac(frame.at) - sourceMac(source))),
            latencyCameraStampToShownMs: summarize(shownPairs.map(({ draw, source }) => toMac(draw.shownAt) - cameraMac(source))),
            // [seconds into the window, ms source -> shown] per drawn frame
            series: shownPairs.map(({ draw, source }) => [round((draw.drawAt - data.start) / 1000, 3), round(toMac(draw.shownAt) - sourceMac(source), 1)]),
        }
    }
    const key = (entry) => entry.key && entry.key.includes(`/${camera}/`)
    const metas = data.meta.filter(key)
    const frames = data.vfc.filter((frame) => key(frame) && inWindow(frame.at))
    const pairs = frames.map((frame) => ({ frame, source: sourceOf(frame.marker, toMac(frame.expectedDisplayTime)) })).filter((pair) => pair.source)
    // Cross-check without the pixel marker (and the only latency for an unmarked camera): the zenoh-web
    // metadata message that arrived closest to the frame's receiveTime, then the relay's last publish at or
    // before that message's sample timestamp.
    const metaPairs = []
    const residuals = []
    for (const frame of frames) {
        if (frame.receiveTime === null) {
            continue
        }
        let best = null
        for (const meta of metas) {
            if (!best || Math.abs(meta.at - frame.receiveTime) < Math.abs(best.at - frame.receiveTime)) {
                best = meta
            }
        }
        if (!best) {
            continue
        }
        residuals.push(Math.abs(best.at - frame.receiveTime))
        let source = null
        for (const candidate of published) {
            if (candidate.publishedMs <= best.timestamp + 2 && best.timestamp - candidate.publishedMs < 100) {
                source = candidate
            }
        }
        if (source && Math.abs(best.at - frame.receiveTime) < 20) {
            metaPairs.push({ frame, meta: best, source })
        }
    }
    const metaInWindow = metas.filter((meta) => inWindow(meta.at))
    const startVideo = data.statsStart.video
    const endVideo = data.statsEnd.video
    const videoDelta = (field) => endVideo.reduce((sum, stat) => sum + (stat[field] ?? 0), 0) - startVideo.reduce((sum, stat) => sum + (stat[field] ?? 0), 0)
    const markers = new Set(frames.map((frame) => frame.marker).filter((marker) => marker !== null))
    return {
        ...common,
        shownFps: perSecond(frames.length),
        distinctFramesShownFps: perSecond(markers.size),
        metadataFps: perSecond(metaInWindow.length),
        markerReadFraction: frames.filter((frame) => frame.marker !== null).length / Math.max(1, frames.length),
        matchedFraction: pairs.length / Math.max(1, frames.length),
        bridgeEncodedBytesPerFrame: mean(metaInWindow.map((meta) => meta.encodedBytes)),
        bridgeMbps: (metaInWindow.reduce((sum, meta) => sum + meta.encodedBytes, 0) * 8) / (windowMs / 1000) / 1e6,
        quality: summarize(metaInWindow.map((meta) => meta.quality)),
        videoSize: metaInWindow.length ? `${metaInWindow.at(-1).width}x${metaInWindow.at(-1).height}` : null,
        latencyShownMs: summarize(pairs.map(({ frame, source }) => toMac(frame.expectedDisplayTime) - sourceMac(source))),
        latencyArrivalMs: summarize(pairs.filter(({ frame }) => frame.receiveTime !== null).map(({ frame, source }) => toMac(frame.receiveTime) - sourceMac(source))),
        latencyCameraStampToShownMs: summarize(pairs.map(({ frame, source }) => toMac(frame.expectedDisplayTime) - cameraMac(source))),
        // [seconds into the window, ms source -> received, ms source -> shown] per composited frame
        series: pairs.map(({ frame, source }) => [round((frame.at - data.start) / 1000, 3), frame.receiveTime === null ? null : round(toMac(frame.receiveTime) - sourceMac(source), 1), round(toMac(frame.expectedDisplayTime) - sourceMac(source), 1)]),
        // [seconds into the window, ms sample timestamp -> metadata message arrival] (the bridge's clock is the robot's)
        metadataSeries: metaInWindow.map((meta) => [round((meta.at - data.start) / 1000, 3), round(toMac(meta.at) - (meta.timestamp - offset.offsetMs), 1)]),
        metadataPairing: {
            matchedFraction: metaPairs.length / Math.max(1, frames.length),
            residualMs: summarize(residuals),
            // how often the metadata pairing names the same frame the pixels do
            agreesWithMarker: (() => {
                const both = metaPairs.filter(({ frame }) => frame.marker !== null && sourceOf(frame.marker, toMac(frame.expectedDisplayTime)))
                return both.length ? both.filter(({ frame, source }) => source.marker === frame.marker).length / both.length : null
            })(),
            latencyShownMs: summarize(metaPairs.map(({ frame, source }) => toMac(frame.expectedDisplayTime) - sourceMac(source))),
            relayToBridgeSampleMs: summarize(metaPairs.map(({ meta, source }) => meta.timestamp - source.publishedMs)),
        },
        // tab-wide (all of this tab's video tracks)
        tab: {
            framesDecodedFps: perSecond(videoDelta("framesDecoded")),
            framesDropped: videoDelta("framesDropped"),
            packetsLost: videoDelta("packetsLost"),
            freezes: videoDelta("freezeCount"),
            videoPayloadBytesPerFrame: videoDelta("bytesReceived") / Math.max(1, videoDelta("framesReceived")),
            videoPayloadMbps: (videoDelta("bytesReceived") * 8) / (windowMs / 1000) / 1e6,
            videoWithRtpHeadersMbps: ((videoDelta("bytesReceived") + videoDelta("headerBytesReceived")) * 8) / (windowMs / 1000) / 1e6,
            dataChannelMbps: ((data.statsEnd.dataBytes - data.statsStart.dataBytes) * 8) / (windowMs / 1000) / 1e6,
            transportMbps: ((data.statsEnd.transportBytes - data.statsStart.transportBytes) * 8) / (windowMs / 1000) / 1e6,
            jitterBufferMsPerFrame: (1000 * videoDelta("jitterBufferDelay")) / Math.max(1, videoDelta("jitterBufferEmittedCount")),
            decodeMsPerFrame: (1000 * videoDelta("totalDecodeTime")) / Math.max(1, videoDelta("framesDecoded")),
            decoder: endVideo.map((stat) => stat.decoderImplementation).join(","),
            iceRttMs: data.statsEnd.rttMs,
        },
    }
}

const before = await clockOffset()
console.log(`clock: robot - mac = ${before.offsetMs.toFixed(1)} ms ± ${before.errorMs.toFixed(1)} (min rtt ${before.rttMinMs.toFixed(1)} ms)`)

const tabs = [{ cameras: [args.camera] }]
if (args["second-tab"]) {
    tabs.push({ cameras: [args["second-camera"]] })
}
let results
try {
    for (const tab of tabs) {
        // one Chrome per viewer: a second tab in the same window is hidden, and a hidden page
        // runs no requestAnimationFrame or requestVideoFrameCallback
        tab.browser = await launch({ headless: true, args: ["--no-sandbox", "--autoplay-policy=no-user-gesture-required"] })
        tab.page = await tab.browser.newPage()
        const celestial = tab.page.unsafelyGetCelestialBindings()
        await celestial.Page.enable()
        await celestial.Page.addScriptToEvaluateOnNewDocument({ source: HOOKS })
        await tab.page.goto(`${args.url}/`, { waitUntil: "load" })
        tab.selected = await selectCameras(tab.page, tab.cameras)
        console.log(`tab ${tabs.indexOf(tab) + 1}: on = ${JSON.stringify(tab.selected)}`)
    }
    await new Promise((resolve) => setTimeout(resolve, Number(args.warmup) * 1000))

    const cpu = sampleCpu(seconds)
    for (const tab of tabs) {
        tab.start = await tab.page.evaluate(async () => {
            window.__measure.statsStart = await window.__measure.stats()
            window.__measure.statsStartAt = performance.now()
            return { start: performance.now(), timeOrigin: performance.timeOrigin, visibility: document.visibilityState }
        })
    }
    await new Promise((resolve) => setTimeout(resolve, seconds * 1000))
    for (const tab of tabs) {
        tab.data = await tab.page.evaluate(async () => {
            const M = window.__measure
            const end = performance.now()
            const statsEnd = await M.stats()
            const tiles = [...document.querySelectorAll(".tile")].map((tile) => tile.querySelector(".tile-bar")?.textContent)
            return { end, statsStart: M.statsStart, statsEnd, ws: M.ws, draws: M.draws, meta: M.meta, vfc: M.vfc, tiles, bridgeStats: M.bridgeStats.filter((entry) => entry.at >= M.statsStartAt).map((entry) => JSON.parse(entry.text)) }
        })
        Object.assign(tab.data, { start: tab.start.start, timeOrigin: tab.start.timeOrigin, visibility: tab.start.visibility })
    }
    const cpuSamples = await cpu
    const after = await clockOffset()
    console.log(`clock after: ${after.offsetMs.toFixed(1)} ms ± ${after.errorMs.toFixed(1)}`)
    const offset = before.errorMs <= after.errorMs ? before : after

    const relayText = await ssh(`cat ${args["frame-log"]}`)
    const relay = relayText.split("\n").filter(Boolean).map((line) => {
        const [channel, crc, bytes, stamp, receivedAt, publishedAt, marker] = line.split(" ")
        return { channel, crc: Number(crc), bytes: Number(bytes), stampMs: Number(stamp) * 1000, receivedMs: Number(receivedAt) * 1000, publishedMs: Number(publishedAt) * 1000, marker: Number(marker) }
    })

    results = {
        label: args.label,
        version: args.version,
        url: args.url,
        seconds,
        at: new Date().toISOString(),
        clock: { before, after, used: offset },
        cpu: { samples: cpuSamples, mean: mean(cpuSamples), p95: quantile(cpuSamples, 0.95) },
        tabs: tabs.map((tab) => ({
            cameras: tab.cameras,
            selected: tab.selected,
            visibility: tab.data.visibility,
            tileBars: tab.data.tiles,
            bridgeStats: tab.data.bridgeStats,
            streams: tab.cameras.map((camera) => analyze({ data: tab.data, version: args.version, camera, relay, offset })),
        })),
    }
} finally {
    for (const tab of tabs) {
        await tab.browser?.close()
    }
}

await Deno.mkdir(args.out, { recursive: true })
const path = `${args.out}/${args.label}.json`
await Deno.writeTextFile(path, JSON.stringify(results, null, 2))
const f = (value, digits = 1) => (value === null || value === undefined || Number.isNaN(value) ? "-" : value.toFixed(digits))
for (const [index, tab] of results.tabs.entries()) {
    for (const stream of tab.streams) {
        const bytes = stream.bytesPerFrame ?? stream.tab?.videoPayloadBytesPerFrame
        const mbps = stream.mbps ?? stream.tab?.videoPayloadMbps
        console.log(`| ${results.label} | tab ${index + 1} ${stream.camera} | ${f(stream.shownFps)} (src ${f(stream.sourceFps)}) | ${f(stream.latencyShownMs.p50, 0)} / ${f(stream.latencyShownMs.p95, 0)} | ${f(stream.latencyArrivalMs.p50, 0)} | ${f(bytes / 1024)} KB | ${f(mbps, 2)} | ${f(results.cpu.mean, 0)}% | matched ${f(100 * stream.matchedFraction, 0)}% |`)
    }
}
console.log(`cpu samples: ${results.cpu.samples.length}, wrote ${path}`)
