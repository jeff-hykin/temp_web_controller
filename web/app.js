import { connect, Priority } from "/vendor/zenoh_web.js"

const element = (id) => document.getElementById(id)

const state = {
    settings: null,
    /// zenoh keys of the cameras open as tiles
    watching: new Set(),
    tiles: new Map(),
    /// every camera the page knows of, by zenoh key: `{ key, label, msgType, encoding, rate }`
    cameras: new Map(),
    /// status topics by zenoh key, for rates and the names web_ctrl gives them
    topicsByKey: new Map(),
    axes: { forward: 0, strafe: 0, turn: 0 },
    keys: new Set(),
    pad: { active: false, x: 0, y: 0 },
    strafeMode: false,
    topics: [],
    lcmError: null,
    shownRecordDir: null,
}

// The server re-sends every setting twice a second, and a status already in flight
// still carries the value you just changed away from. Rendering it would drag the
// control back under your finger. So a setting you changed wins until the server
// echoes it back, and only then does the server become the source of truth again.
const pendingSettings = new Map()
// Some values are legitimately refused — an unusable publish topic leaves the old
// one standing — and without a deadline the control would sit on a lie forever.
const SETTING_ECHO_GRACE_MS = 2000

function sendSetting(key, value) {
    pendingSettings.set(key, { value, sentAt: performance.now() })
    state.settings[key] = value
    send({ type: "settings", [key]: value })
}

function withPendingSettings(settings) {
    for (const [key, pending] of pendingSettings) {
        if (settings[key] === pending.value || performance.now() - pending.sentAt > SETTING_ECHO_GRACE_MS) {
            pendingSettings.delete(key)
        } else {
            settings[key] = pending.value
        }
    }
    return settings
}

let control = null

// Presses made while the link is down are held rather than dropped: a Stop Recording
// that lands in the second between a dropped socket and its retry used to vanish with
// no sign, which reads exactly like the button not working.
const pendingCommands = []

function connectControl() {
    const protocol = location.protocol === "https:" ? "wss:" : "ws:"
    control = new WebSocket(`${protocol}//${location.host}/ws`)
    control.addEventListener("open", () => {
        element("link-dot").classList.add("live")
        while (pendingCommands.length > 0) {
            control.send(JSON.stringify(pendingCommands.shift()))
        }
        // Those settings have only just been asked for, however long the link was
        // down, so their echo deadline starts now.
        for (const pending of pendingSettings.values()) {
            pending.sentAt = performance.now()
        }
    })
    control.addEventListener("close", () => {
        element("link-dot").classList.remove("live")
        control = null
        setTimeout(connectControl, 1000)
    })
    control.addEventListener("message", (event) => applyStatus(JSON.parse(event.data)))
}

function send(payload) {
    if (control && control.readyState === WebSocket.OPEN) {
        control.send(JSON.stringify(payload))
    } else {
        pendingCommands.push(payload)
    }
}

function applyStatus(status) {
    if (status.type !== "status") {
        return
    }
    const publish = status.publish
    const transports = ["zenoh", publish.lcm && "lcm"].filter(Boolean).join(" + ")
    const target = element("publish-target")
    target.textContent = `driving ${publish.topic} over ${transports}`
    if (zenoh.client?.state !== "connected") {
        target.textContent += ` — zenoh-web ${zenoh.client?.state ?? "connecting"}`
    }
    if (drive.publisher?.state === "rejected") {
        target.textContent += ` — ${drive.publisher.rejectionReason ?? "command topic refused"}`
    }
    state.zenohWeb = status.zenoh_web

    const lcmError = status.receivers?.lcm_error
    state.lcmError = lcmError
    target.classList.toggle("bad", Boolean(lcmError))
    if (lcmError) {
        target.textContent += ` — not hearing lcm: ${lcmError}`
    }

    // The saved-file list is a listing of whatever directory the server is
    // recording into, so it goes stale the moment that directory changes.
    if (state.shownRecordDir !== status.settings.record_dir) {
        state.shownRecordDir = status.settings.record_dir
        pollRecordings()
    }

    state.settings = withPendingSettings(status.settings)
    state.topicsByKey = new Map(status.topics.map((topic) => [topic.key, topic]))
    renderSettings(state.settings)
    keepZenohConnected()
    keepDriving()
    refreshTileSubscriptions()
    renderCameras()
    renderTopics(status.topics)
    renderTileStats()
    renderRecording(status.recording, status.topics)
    renderLauncher(status.launcher)
    renderValues()
}

const setText = (node, text) => {
    if (node.textContent !== text) {
        node.textContent = text
    }
}

/// Rebuilding a list on every poll destroys the node under your finger, so a press
/// in flight lands on nothing and local feedback ("starting…", a half-typed field)
/// is wiped twice a second. Keep one node per key and only write what changed.
/// `create(item)` returns a node carrying an `update(item)` method.
function reconcile(container, items, keyOf, create) {
    const existing = new Map()
    for (const child of [...container.children]) {
        if (child.dataset.rowKey === undefined) {
            child.remove()
        } else {
            existing.set(child.dataset.rowKey, child)
        }
    }
    let previous = null
    for (const item of items) {
        const key = keyOf(item)
        let node = existing.get(key)
        if (node) {
            existing.delete(key)
        } else {
            node = create(item)
            node.dataset.rowKey = key
        }
        node.update(item)
        const wanted = previous ? previous.nextSibling : container.firstChild
        if (node !== wanted) {
            container.insertBefore(node, wanted)
        }
        previous = node
    }
    for (const node of existing.values()) {
        node.remove()
    }
}

/// The last launcher state the server reported, kept so a save or a delete can
/// redraw the list immediately instead of looking ignored until the next poll.
let launcherView = null
// Not 0: performance.now() starts near zero, so that would read as a press that
// just happened and the button would load already stuck on "Killing…".
let killPressedAt = -Infinity

/// Derived from a timestamp rather than unwound by a timer, because a backgrounded
/// tab throttles timers and would leave the button stranded on "Killing…".
function renderKillButton() {
    const killing = performance.now() - killPressedAt < 2000
    const button = element("launch-kill")
    button.disabled = killing
    button.textContent = killing ? "Killing…" : "Kill blueprint"
}

function renderLauncher(launcher) {
    if (!launcher) {
        return
    }
    launcherView = launcher
    const running = launcher.running
    const failed = !running && launcher.finished && launcher.finished.code !== 0
    element("launch-state").textContent = running
        ? `${running.name} · ${running.seconds.toFixed(0)}s`
        : failed
            ? `${launcher.finished.name} ${launcher.finished.code === null ? "was killed" : `failed (${launcher.finished.code})`}`
            : "idle"
    element("launch-state").classList.toggle("failed", Boolean(failed))
    element("launch-open").classList.toggle("running", Boolean(running))
    renderKillButton()

    const output = element("launch-output")
    const text = launcher.lines.length ? launcher.lines.join("\n") : "nothing launched yet"
    output.classList.toggle("failed", Boolean(failed))
    // Rewriting the text node drops any selection, and forcing the scroll every poll
    // yanks the view back down while you are reading further up. So only follow the
    // tail when already parked at the bottom, and only touch the DOM on a change.
    if (output.textContent !== text) {
        const atBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 24
        output.textContent = text
        if (atBottom) {
            output.scrollTop = output.scrollHeight
        }
    }

    const list = element("launch-list")
    if (!launcher.commands.length) {
        list.replaceChildren(Object.assign(document.createElement("p"), {
            className: "hint-text",
            textContent: "no saved commands yet",
        }))
        return
    }
    reconcile(list, launcher.commands, (saved) => saved.name, () => {
        const row = document.createElement("div")
        row.className = "launch-row"
        const label = document.createElement("span")
        // The command itself is worth showing, since a tooltip is unreachable on
        // the phone this is mostly used from.
        const detail = document.createElement("em")
        const run = document.createElement("button")
        run.className = "launch-run"
        run.textContent = "Launch"
        let pressedAt = -Infinity
        run.addEventListener("click", () => {
            send({ type: "launch_run", name: row.dataset.rowKey })
            // Nothing comes back until the command produces its first line, so say
            // out loud that the press landed.
            pressedAt = performance.now()
            run.classList.add("starting")
            element("launch-state").textContent = `starting ${row.dataset.rowKey}…`
            element("launch-state").classList.remove("failed")
        })
        const stop = document.createElement("button")
        stop.className = "ghost small"
        stop.addEventListener("click", () => {
            const name = row.dataset.rowKey
            if (launcherView?.running?.name === name) {
                send({ type: "launch_stop" })
                return
            }
            send({ type: "launch_delete", name })
            renderLauncher({
                ...launcherView,
                commands: launcherView.commands.filter((other) => other.name !== name),
            })
        })
        row.append(label, detail, run, stop)
        row.update = (saved) => {
            setText(label, saved.name)
            setText(detail, saved.command)
            const live = launcherView?.running
            run.disabled = Boolean(live)
            // The node now outlives the press, so the flash needs its own expiry.
            run.classList.toggle("starting", !live && performance.now() - pressedAt < 8000)
            const isRunning = live?.name === saved.name
            setText(stop, isRunning ? "Stop" : "Delete")
            stop.classList.toggle("danger", !isRunning)
        }
        return row
    })
}

const formatBytes = (bytes) => {
    if (bytes < 1024) {
        return `${bytes} B`
    }
    if (bytes < 1024 * 1024) {
        return `${(bytes / 1024).toFixed(0)} KB`
    }
    if (bytes < 1024 * 1024 * 1024) {
        return `${(bytes / 1024 / 1024).toFixed(1)} MB`
    }
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`
}

const formatAge = (seconds) => {
    if (seconds < 90) {
        return `${seconds.toFixed(0)}s ago`
    }
    if (seconds < 3600) {
        return `${(seconds / 60).toFixed(0)}m ago`
    }
    if (seconds < 86400) {
        return `${(seconds / 3600).toFixed(0)}h ago`
    }
    return `${(seconds / 86400).toFixed(0)}d ago`
}

function renderRecording(recording, topics) {
    if (!recording) {
        return
    }
    const wasRecording = state.recording?.active
    state.recording = recording

    const toggle = element("record-toggle")
    toggle.textContent = recording.active ? "Stop recording" : "Start recording"
    toggle.classList.toggle("on", recording.active)

    const open = element("record-open")
    open.textContent = recording.active ? formatBytes(recording.bytes) : "Record"
    open.classList.toggle("recording", recording.active)
    element("record-badge").hidden = !recording.active

    const dropped = recording.dropped > 0 ? ` · ${recording.dropped} dropped` : ""
    element("record-stats").textContent = recording.active
        ? `${recording.path.split("/").pop()} · ${recording.seconds.toFixed(0)}s · ${recording.messages} msgs · ${formatBytes(recording.bytes)}${dropped}`
        : "idle"

    renderRecordTopics(topics)
    // A finished file only shows up in the listing once it is closed.
    if (wasRecording && !recording.active) {
        pollRecordings()
    }
}

/// Only topics somebody actually toggled are remembered, so a topic that shows
/// up for the first time still lands on its default rather than on a stale answer.
const OVERRIDES_KEY = "web_ctrl.topic_overrides"

function topicOverrides() {
    try {
        const stored = JSON.parse(localStorage.getItem(OVERRIDES_KEY))
        return stored?.constructor === Object ? stored : {}
    } catch {
        return {}
    }
}

function wantsRecording(topic, overrides) {
    return overrides[topic.topic] ?? !topic.is_rpc
}

function setTopicRecorded(topics, recorded) {
    const overrides = topicOverrides()
    for (const topic of topics) {
        overrides[topic.topic] = recorded
    }
    localStorage.setItem(OVERRIDES_KEY, JSON.stringify(overrides))
    for (const topic of topics) {
        send({ type: "record_topic", topic: topic.topic, recorded })
        topic.recorded = recorded
    }
    renderRecordTopics(state.topics)
}

let rpcExpanded = false

function renderRecordTopics(topics) {
    state.topics = topics
    const overrides = topicOverrides()
    // The server forgets the overrides on restart, so re-assert any topic whose
    // live state has drifted from what this browser asked for.
    for (const topic of topics) {
        const wanted = wantsRecording(topic, overrides)
        if (topic.recorded !== wanted) {
            send({ type: "record_topic", topic: topic.topic, recorded: wanted })
        }
    }

    const rpc = topics.filter((topic) => topic.is_rpc)
    const rows = topics
        .filter((topic) => !topic.is_rpc)
        .map((topic) => ({ topic, checked: wantsRecording(topic, overrides) }))
    if (rpc.length > 0) {
        rows.push({ rpc, on: rpc.filter((topic) => wantsRecording(topic, overrides)).length })
        if (rpcExpanded) {
            rows.push(...rpc.map((topic) => ({ topic, checked: wantsRecording(topic, overrides) })))
        }
    }
    const keyOf = (item) => item.rpc ? RPC_HEAD_KEY : item.topic.topic
    const create = (item) => item.rpc ? rpcHeaderRow() : recordTopicRow()
    reconcile(element("record-topics"), rows, keyOf, create)
}

/// A sentinel rather than a topic name, since the header is not a topic and no
/// topic may collide with it.
const RPC_HEAD_KEY = " rpc-head"

function recordTopicRow() {
    const row = document.createElement("label")
    row.className = "record-topic"
    const box = document.createElement("input")
    box.type = "checkbox"
    box.addEventListener("change", () => setTopicRecorded([row.topic], box.checked))
    const name = document.createElement("span")
    const type = document.createElement("em")
    row.append(box, name, type)
    row.update = (item) => {
        row.topic = item.topic
        setText(name, item.topic.topic)
        setText(type, item.topic.msg_type ?? "?")
        box.checked = item.checked
    }
    return row
}

function rpcHeaderRow() {
    const row = document.createElement("div")
    row.className = "record-topic rpc-head"

    const box = document.createElement("input")
    box.type = "checkbox"
    box.addEventListener("change", () => setTopicRecorded(row.rpc, box.checked))

    const name = document.createElement("span")
    const label = document.createElement("label")
    label.append(box, name)

    const expand = document.createElement("button")
    expand.className = "ghost small"
    expand.addEventListener("click", () => {
        rpcExpanded = !rpcExpanded
        renderRecordTopics(state.topics)
    })

    row.append(label, expand)
    row.update = (item) => {
        row.rpc = item.rpc
        setText(name, `RPC topics (${item.on}/${item.rpc.length})`)
        box.checked = item.on === item.rpc.length
        box.indeterminate = item.on > 0 && item.on < item.rpc.length
        setText(expand, rpcExpanded ? "Hide" : "Show")
    }
    return row
}

async function pollRecordings() {
    let files = []
    try {
        files = await (await fetch("/api/recordings")).json()
    } catch {
        return
    }
    const list = element("record-files")
    if (files.length === 0) {
        list.replaceChildren(Object.assign(document.createElement("p"), {
            className: "hint-text",
            textContent: "nothing recorded yet",
        }))
        return
    }
    list.replaceChildren(...files.map((file) => {
        const row = document.createElement("div")
        row.className = "record-file"

        const name = document.createElement("span")
        name.textContent = file.name
        const meta = document.createElement("em")
        meta.textContent = `${formatBytes(file.bytes)} · ${formatAge(file.seconds_old)}`

        const copy = document.createElement("button")
        copy.className = "ghost small"
        copy.textContent = "Path"
        copy.addEventListener("click", async () => {
            await copyText(file.path)
            copy.textContent = "Copied"
            setTimeout(() => { copy.textContent = "Path" }, 1200)
        })

        const remove = document.createElement("button")
        remove.className = "ghost small danger"
        remove.textContent = "Delete"
        remove.addEventListener("click", async () => {
            if (remove.dataset.armed !== "yes") {
                remove.dataset.armed = "yes"
                remove.textContent = "Sure?"
                setTimeout(() => {
                    remove.dataset.armed = "no"
                    remove.textContent = "Delete"
                }, 3000)
                return
            }
            await fetch(`/api/recordings/${encodeURIComponent(file.name)}`, { method: "DELETE" })
            pollRecordings()
        })

        row.append(name, meta, copy, remove)
        return row
    }))
}

/// The clipboard API needs a secure context, which a plain LAN http page is
/// not, so fall back to a throwaway selection.
async function copyText(text) {
    try {
        await navigator.clipboard.writeText(text)
        return
    } catch {
        const scratch = document.createElement("textarea")
        scratch.value = text
        scratch.style.position = "fixed"
        scratch.style.opacity = "0"
        document.body.append(scratch)
        scratch.select()
        document.execCommand("copy")
        scratch.remove()
    }
}

function renderValues() {
    const settings = state.settings
    if (!settings) {
        return
    }
    const linear = state.axes.forward * settings.linear_speed
    const angular = state.axes.turn * settings.angular_speed * (settings.invert_turn ? -1 : 1)
    element("value-linear").textContent = linear.toFixed(2)
    element("value-angular").textContent = angular.toFixed(2)
}

/// Cameras come from zenoh-web's topic listing, which sees every lcm channel web_ctrl
/// relays (each holds a liveliness token) and any zenoh publisher that declares
/// itself. A zenoh camera that only ever puts is invisible to a listing that does not
/// subscribe, so the image topics web_ctrl's own catch-all heard are merged in.
function renderCameras() {
    const cameras = new Map()
    for (const key of state.listedKeys ?? []) {
        const msgType = imageTypeOf(key)
        if (msgType) {
            cameras.set(key, { key, msgType })
        }
    }
    for (const topic of state.topicsByKey.values()) {
        if (topic.is_image && !cameras.has(topic.key)) {
            cameras.set(topic.key, { key: topic.key, msgType: topic.msg_type })
        }
    }
    for (const camera of cameras.values()) {
        const topic = state.topicsByKey.get(camera.key)
        camera.label = topic?.topic ?? camera.key.replace(/^dimos\//, "").replace(/\/[^/]*$/, "")
        camera.encoding = topic?.encoding ?? null
        camera.rate = topic?.rate ?? 0
    }
    state.cameras = cameras

    const picker = element("camera-picker")
    for (const chip of [...picker.children]) {
        if (!cameras.has(chip.dataset.key)) {
            chip.remove()
        }
    }
    const sorted = [...cameras.values()].sort((left, right) => left.label.localeCompare(right.label))
    for (const camera of sorted) {
        let chip = picker.querySelector(`[data-key="${CSS.escape(camera.key)}"]`)
        if (!chip) {
            chip = document.createElement("button")
            chip.className = "chip"
            chip.dataset.key = camera.key
            chip.addEventListener("click", () => toggleStream(camera.key))
            // The rate lives in its own fixed-width span: written inline, every
            // 9→10 hz tick resized the chip and shoved its neighbours sideways.
            chip.append(
                Object.assign(document.createElement("span"), { textContent: `${camera.label} · ` }),
                Object.assign(document.createElement("span"), { className: "hz" }),
            )
            picker.append(chip)
        }
        setText(chip.lastChild, `${camera.rate.toFixed(0)}hz`)
        chip.classList.toggle("on", state.watching.has(camera.key))
    }
    if (sorted.length && state.watching.size === 0 && !state.closedByHand) {
        toggleStream(sorted[0].key)
    }
    element("streams-empty").hidden = state.watching.size > 0
}

const IMAGE_TYPES = ["sensor_msgs.Image", "sensor_msgs.CompressedImage"]

const imageTypeOf = (key) => IMAGE_TYPES.find((msgType) => key.endsWith(`/${msgType}`)) ?? null

/// Depth goes to a canvas, losslessly; anything else becomes H.264 video. The pixel
/// encoding decides when web_ctrl has read a frame, the name when it has not (an
/// unwatched fragmented lcm image is never reassembled, so its encoding is unknown).
function codecFor(camera) {
    const depthEncodings = ["16UC1", "32FC1", "mono16"]
    const looksDepth = camera.encoding
        ? depthEncodings.includes(camera.encoding) && (camera.encoding !== "mono16" || /depth/i.test(camera.key))
        : /depth/i.test(camera.key)
    if (camera.msgType === "sensor_msgs.CompressedImage") {
        return looksDepth ? "dimos-compressed-depth" : "dimos-compressed-image"
    }
    return looksDepth ? "dimos-depth" : "dimos-image"
}

let discovering = false

async function discoverCameras() {
    const client = zenoh.client
    if (discovering || !client || client.state !== "connected") {
        return
    }
    discovering = true
    try {
        // probeMs 0: declarations and tokens only. A probe would subscribe to every
        // key for a moment and wake each lazily relayed camera for nothing.
        const listed = await client.listTopics("**", { probeMs: 0 })
        state.listedKeys = listed.map((topic) => topic.key)
        renderCameras()
    } catch (error) {
        console.warn("listing topics failed", error)
    } finally {
        discovering = false
    }
}

function renderTopics(topics) {
    reconcile(element("topic-table"), topics, (topic) => topic.topic, () => {
        const row = document.createElement("div")
        row.className = "topic-row"
        const name = document.createElement("span")
        const type = document.createElement("span")
        type.className = "type"
        const rate = document.createElement("span")
        rate.className = "rate"
        row.append(name, type, rate)
        row.update = (topic) => {
            row.classList.toggle("stale", topic.seconds_since_seen > 5)
            setText(name, topic.topic)
            // A frame we could not classify is still recorded, so the count is the
            // only place the defect shows up before someone opens the file.
            setText(type, topic.unclassifiable
                ? `${topic.msg_type ?? "?"} · ${topic.unclassifiable} unreadable`
                : topic.msg_type ?? "?")
            type.classList.toggle("bad", topic.unclassifiable > 0)
            setText(rate, `${topic.rate.toFixed(0)} hz`)
        }
        return row
    })
}

/// Driving is comfortable under 150 ms and visibly laggy past 300.
const LATENCY_GOOD_MS = 150
const LATENCY_HIGH_MS = 300
/// A frame older than this is not worth sending: the bridge drops it instead.
const FRAME_MAX_AGE_MS = 500

/// How camera pictures reach the page: "video" (zenoh-web's H.264 track, shown in a <video>)
/// or "jpeg" (one JPEG file per picture on the data channel, drawn on a canvas). `?imageTransport=`
/// in the page's URL overrides the default. On R1 (a Jetson Orin, Chrome over a jittery Wi-Fi +
/// VPN path) H.264 measured ~27 fps against ~5 fps for JPEG files, whose data channel could not
/// hold the rate on that path, so video is the default.
const DEFAULT_IMAGE_TRANSPORT = "video"
const IMAGE_TRANSPORT = ["video", "jpeg"].includes(new URLSearchParams(location.search).get("imageTransport"))
    ? new URLSearchParams(location.search).get("imageTransport")
    : DEFAULT_IMAGE_TRANSPORT
/// A frozen last frame reads exactly like a live one, so a feed that stopped is
/// blanked rather than left showing whatever it was pointing at minutes ago. The
/// window scales with the topic's own rate so a genuinely slow publisher does not
/// blink offline between frames.
const OFFLINE_FLOOR_MS = 3000

function renderTileStats() {
    for (const tile of state.tiles.values()) {
        // Only this side knows how many frames actually made it onto the screen: a
        // phone whose decoder is the bottleneck receives everything and shows a
        // fraction of it, which looks identical to a healthy stream from the robot.
        const since = performance.now() - tile.countedAt
        if (since >= 1000) {
            tile.paintedFps = tile.painted / (since / 1000)
            tile.painted = 0
            tile.countedAt = performance.now()
        }
        const rate = state.cameras.get(tile.key)?.rate ?? 0
        const window = Math.max(OFFLINE_FLOOR_MS, rate > 0 ? 4000 / rate : 0)
        const offline = tile.paintedAt === 0 || performance.now() - tile.paintedAt > window
        tile.root.classList.toggle("offline", offline)
        if (tile.refusal) {
            setText(tile.info, tile.refusal)
            continue
        }
        if (offline) {
            if (tile.media instanceof HTMLCanvasElement && tile.media.width > 0) {
                tile.media.getContext("2d").clearRect(0, 0, tile.media.width, tile.media.height)
            }
            tile.latency.hidden = true
            setText(tile.info, tile.paintedAt === 0 ? "waiting for frames" : "offline")
            continue
        }
        // The first number is what this browser drew, not what the robot sent: a
        // tile that says 29 fps while showing 5 is the bug this whole reading is for.
        const frame = tile.lastFrame
        let detail = ""
        if (frame?.video) {
            const video = frame.video
            detail = ` · ${video.width}x${video.height} q${Math.round(video.quality * 100)} · ${(video.encodedBytes / 1024).toFixed(0)} KB`
        } else if (tile.imageSize) {
            detail = ` · ${tile.imageSize} jpeg · ${(tile.imageBytes / 1024).toFixed(0)} KB`
        } else if (frame?.depth) {
            const depth = frame.depth
            detail = ` · ${depth.width}x${depth.height} depth${depth.stride > 1 ? ` 1/${depth.stride}` : ""}`
        }
        setText(tile.info, `${tile.paintedFps.toFixed(0)}/${rate.toFixed(0)} fps${detail}`)
        const latency = tile.latencyMs
        tile.latency.hidden = latency === null
        setText(tile.latency, latency === null ? "" : `${latency.toFixed(0)} ms`)
        tile.latency.classList.toggle("bad", latency > LATENCY_HIGH_MS)
        tile.latency.classList.toggle("warn", latency > LATENCY_GOOD_MS && latency <= LATENCY_HIGH_MS)
    }
}

const NODE_HEIGHT = 30
const ROW_GAP = 74
const NODE_GAP = 22
const GRAPH_MARGIN = 22

const svgElement = (name, attributes) => {
    const node = document.createElementNS("http://www.w3.org/2000/svg", name)
    for (const [key, value] of Object.entries(attributes)) {
        node.setAttribute(key, value)
    }
    return node
}

/// Places every frame on the row below its parent, then draws the edges. Frames
/// only reachable through a cycle get their own rows below everything else.
function layoutTf(view) {
    const childrenOf = new Map()
    const parentsOf = new Map()
    const frames = new Set()
    for (const link of view.links) {
        childrenOf.set(link.parent, [...(childrenOf.get(link.parent) ?? []), link])
        parentsOf.set(link.child, [...(parentsOf.get(link.child) ?? []), link])
        frames.add(link.parent)
        frames.add(link.child)
    }

    // Longest path from a root, so a frame always sits strictly below every one
    // of its parents. The round cap is what stops a cycle from running away.
    const depths = new Map([...frames].map((frame) => [frame, 0]))
    for (let round = 0; round < frames.size; round++) {
        let changed = false
        for (const link of view.links) {
            if (depths.get(link.child) < depths.get(link.parent) + 1) {
                depths.set(link.child, depths.get(link.parent) + 1)
                changed = true
            }
        }
        if (!changed) {
            break
        }
    }
    const used = [...new Set(depths.values())].sort((left, right) => left - right)
    for (const [frame, level] of depths) {
        depths.set(frame, used.indexOf(level))
    }

    const rows = new Map()
    for (const [frame, level] of depths) {
        rows.set(level, [...(rows.get(level) ?? []), frame])
    }

    const placed = new Map()
    let widest = 0
    const levels = [...rows.keys()].sort((left, right) => left - right)
    for (const level of levels) {
        const anchor = (frame) => {
            const centers = (parentsOf.get(frame) ?? [])
                .map((link) => placed.get(link.parent))
                .filter(Boolean)
                .map((box) => box.x + box.width / 2)
            return centers.length ? centers.reduce((sum, value) => sum + value, 0) / centers.length : 0
        }
        const row = rows.get(level).sort((left, right) => anchor(left) - anchor(right) || left.localeCompare(right))
        let offset = 0
        for (const frame of row) {
            const width = Math.max(66, frame.length * 7.2 + 22)
            placed.set(frame, { x: offset, y: level * ROW_GAP, width })
            offset += width + NODE_GAP
        }
        widest = Math.max(widest, offset - NODE_GAP)
    }
    for (const level of levels) {
        const row = rows.get(level)
        const last = placed.get(row[row.length - 1])
        const shift = (widest - (last.x + last.width)) / 2
        for (const frame of row) {
            placed.get(frame).x += shift
        }
    }

    const orphans = new Set([...frames].filter((frame) => !isReachable(frame, view.roots, parentsOf)))
    return { placed, parentsOf, orphans, width: widest, height: levels.length * ROW_GAP - ROW_GAP + NODE_HEIGHT }
}

function isReachable(frame, roots, parentsOf) {
    const seen = new Set()
    const queue = [frame]
    while (queue.length) {
        const current = queue.pop()
        if (roots.includes(current)) {
            return true
        }
        if (!seen.add(current)) {
            continue
        }
        queue.push(...(parentsOf.get(current) ?? []).map((link) => link.parent))
    }
    return false
}

function renderTfGraph(view) {
    const container = element("tf-graph")
    if (view.links.length === 0) {
        const empty = document.createElement("p")
        empty.className = "empty-graph"
        empty.textContent = "no tf seen yet"
        container.replaceChildren(empty)
        return
    }

    const { placed, parentsOf, orphans, width, height } = layoutTf(view)
    const svg = svgElement("svg", {
        width: width + GRAPH_MARGIN * 2,
        height: height + GRAPH_MARGIN * 2,
        viewBox: `${-GRAPH_MARGIN} ${-GRAPH_MARGIN} ${width + GRAPH_MARGIN * 2} ${height + GRAPH_MARGIN * 2}`,
    })

    for (const link of view.links) {
        const from = placed.get(link.parent)
        const to = placed.get(link.child)
        const doubleParent = (parentsOf.get(link.child) ?? []).length > 1
        const startX = from.x + from.width / 2
        const startY = from.y + NODE_HEIGHT
        const endX = to.x + to.width / 2
        const endY = to.y
        const bend = Math.max(18, Math.abs(endY - startY) / 2)
        let className = "edge"
        if (link.stale) {
            className += " stale"
        } else if (doubleParent) {
            className += " bad"
        }
        svg.append(svgElement("path", {
            class: className,
            d: `M ${startX} ${startY} C ${startX} ${startY + bend}, ${endX} ${endY - bend}, ${endX} ${endY}`,
            "marker-end": "url(#tf-arrow)",
        }))
        if (link.stale || link.is_static) {
            const label = svgElement("text", {
                class: link.stale ? "edge-label bad" : "edge-label",
                x: (startX + endX) / 2,
                y: (startY + endY) / 2 + 4,
            })
            label.textContent = link.stale ? `${link.seconds_since_seen.toFixed(0)}s ago` : "static"
            svg.append(label)
        }
    }

    for (const [frame, box] of placed) {
        const broken = orphans.has(frame) || (parentsOf.get(frame) ?? []).length > 1
        const group = svgElement("g", {
            class: `node${broken ? " bad" : view.roots.includes(frame) ? " root" : ""}`,
        })
        group.append(svgElement("rect", { x: box.x, y: box.y, width: box.width, height: NODE_HEIGHT }))
        const label = svgElement("text", { x: box.x + box.width / 2, y: box.y + NODE_HEIGHT / 2 })
        label.textContent = frame
        group.append(label)
        svg.append(group)
    }

    const arrow = svgElement("marker", {
        id: "tf-arrow",
        viewBox: "0 0 8 8",
        refX: 7,
        refY: 4,
        markerWidth: 6,
        markerHeight: 6,
        orient: "auto-start-reverse",
        markerUnits: "userSpaceOnUse",
    })
    arrow.append(svgElement("path", { d: "M 0 0 L 8 4 L 0 8 z", fill: "rgba(235, 235, 245, 0.45)" }))
    const defs = svgElement("defs", {})
    defs.append(arrow)
    svg.prepend(defs)

    container.replaceChildren(svg)
}

function renderTf(view) {
    const warnings = element("tf-warnings")
    warnings.hidden = view.warnings.length === 0
    warnings.replaceChildren(...view.warnings.map((text) => {
        const line = document.createElement("span")
        line.textContent = text
        return line
    }))
    element("settings-badge").hidden = view.warnings.length === 0 && !state.lcmError

    const summary = element("tf-summary")
    summary.classList.toggle("bad", view.warnings.length > 0)
    if (view.links.length === 0) {
        summary.textContent = "no tf seen yet"
    } else if (view.warnings.length > 0) {
        summary.textContent = `${view.warnings.length} problem${view.warnings.length > 1 ? "s" : ""}`
    } else {
        summary.textContent = `${view.links.length} transforms, healthy`
    }

    renderTfGraph(view)
}

async function pollTf() {
    try {
        renderTf(await (await fetch("/api/tf")).json())
    } catch {
        // The control socket already reports the link being down.
    }
}

function toggleStream(key) {
    if (state.watching.has(key)) {
        state.watching.delete(key)
        // Closing every tile by hand must not have the first camera pop back open.
        state.closedByHand = state.watching.size === 0
        const tile = state.tiles.get(key)
        if (tile) {
            state.tiles.delete(key)
            tile.subscription?.close()
            tile.root.remove()
        }
    } else {
        state.watching.add(key)
        openTile(key)
    }
    renderCameras()
}

function openTile(key) {
    const camera = state.cameras.get(key) ?? { key, label: key, msgType: imageTypeOf(key) }
    const codec = codecFor(camera)
    const isDepth = codec.endsWith("-depth")
    const root = document.createElement("div")
    root.className = "tile"
    const media = isDepth || IMAGE_TRANSPORT === "jpeg"
        ? document.createElement("canvas")
        : Object.assign(document.createElement("video"), { muted: true, autoplay: true, playsInline: true })
    const bar = document.createElement("div")
    bar.className = "tile-bar"
    const name = document.createElement("strong")
    name.textContent = camera.label
    const info = document.createElement("span")
    info.textContent = "connecting"
    const latency = document.createElement("span")
    bar.append(name, info, latency)
    root.append(media, bar)
    element("streams").append(root)

    const tile = {
        key, codec, root, media, info, latency,
        subscription: null, optionsSignature: null, refusal: null,
        paintedAt: 0, painted: 0, paintedFps: 0, countedAt: performance.now(),
        lastFrame: null, latencyMs: null,
    }
    root.dataset.key = key
    state.tiles.set(key, tile)
    if (media instanceof HTMLVideoElement) {
        countVideoFrames(tile)
    }
    subscribeTile(tile)
}

/// requestVideoFrameCallback fires once per frame actually composited, which is the
/// only honest frame rate for a <video>.
function countVideoFrames(tile) {
    const video = tile.media
    if (!video.requestVideoFrameCallback) {
        return
    }
    const onFrame = () => {
        tile.framesComposited = true
        tile.painted += 1
        tile.paintedAt = performance.now()
        video.requestVideoFrameCallback(onFrame)
    }
    video.requestVideoFrameCallback(onFrame)
}

/// The camera settings, as zenoh-web subscribe options. Quality is the best a camera
/// is sent at; with auto quality on, zenoh-web may lower it to a tenth to fit the
/// link, and the tradeoff says whether it gives up sharpness or frames first.
function subscribeOptions(codec) {
    const settings = state.settings
    const best = settings.quality / 100
    const options = {
        codec,
        maxQuality: best,
        minQuality: settings.auto_quality ? Math.min(0.1, best) : best,
        qualityToHzTradeoff: settings.quality_to_hz_tradeoff,
        maxAge: FRAME_MAX_AGE_MS,
    }
    if (settings.max_hz > 0) {
        options.maxHz = settings.max_hz
    }
    if (!codec.endsWith("-depth")) {
        options.imageTransport = IMAGE_TRANSPORT
    }
    return options
}

function subscribeTile(tile) {
    tile.subscription?.close()
    tile.subscription = null
    const client = zenoh.client
    if (!client || !state.settings) {
        return
    }
    const options = subscribeOptions(tile.codec)
    tile.optionsSignature = JSON.stringify(options)
    tile.refusal = null
    const subscription = client.subscribe(tile.key, options, (message) => onTileMessage(tile, message))
    tile.subscription = subscription
    subscription.ready().catch((error) => {
        if (tile.subscription === subscription) {
            tile.refusal = `refused: ${error.message}`
        }
    })
}

/// Options only change by closing and subscribing again, so this runs on every
/// status and is a no-op unless a camera setting actually moved.
function refreshTileSubscriptions() {
    if (!state.settings) {
        return
    }
    for (const tile of state.tiles.values()) {
        if (!tile.subscription || JSON.stringify(subscribeOptions(tile.codec)) !== tile.optionsSignature) {
            subscribeTile(tile)
        }
    }
}

function onTileMessage(tile, message) {
    tile.lastFrame = message
    const client = zenoh.client
    // The frame's stamp is on the bridge's clock, so shift it onto ours.
    if (client && client.clockOffsetMs !== null) {
        const age = client.now() + client.clockOffsetMs - message.timestamp
        tile.latencyMs = age >= 0 && age < 10000 ? age : null
    }
    if (message.mediaStream && tile.media.srcObject !== message.mediaStream) {
        tile.media.srcObject = message.mediaStream
        tile.media.play().catch(() => {})
    }
    // Each video frame also brings its metadata here. Where requestVideoFrameCallback
    // never fires (no compositor, an old browser) that arrival is the best sign of life.
    if (message.video && !tile.framesComposited) {
        tile.painted += 1
        tile.paintedAt = performance.now()
    }
    if (message.depth) {
        drawDepth(tile, message.depth)
        tile.painted += 1
        tile.paintedAt = performance.now()
    }
    if (message.image) {
        const canvas = tile.media
        if (canvas.width !== message.image.width || canvas.height !== message.image.height) {
            canvas.width = message.image.width
            canvas.height = message.image.height
        }
        canvas.getContext("2d").drawImage(message.image, 0, 0)
        tile.imageSize = `${message.image.width}x${message.image.height}`
        tile.imageBytes = message.bytes.length
        message.image.close()
        tile.painted += 1
        tile.paintedAt = performance.now()
    }
}

const TURBO_STOPS = [
    [0.19, 0.07, 0.23],
    [0.11, 0.53, 0.90],
    [0.14, 0.87, 0.68],
    [0.68, 0.98, 0.24],
    [0.98, 0.68, 0.12],
    [0.73, 0.09, 0.03],
]

const TURBO = (() => {
    const table = new Uint8Array(256 * 3)
    for (let index = 0; index < 256; index++) {
        const position = (index / 255) * (TURBO_STOPS.length - 1)
        const low = Math.floor(position)
        const high = Math.min(low + 1, TURBO_STOPS.length - 1)
        const blend = position - low
        for (let channel = 0; channel < 3; channel++) {
            const value = TURBO_STOPS[low][channel] * (1 - blend) + TURBO_STOPS[high][channel] * blend
            table[index * 3 + channel] = Math.round(value * 255)
        }
    }
    return table
})()

/// Near is blue, far is red, stretched over this frame's own range; a missing
/// reading (0, or not finite) stays black.
function drawDepth(tile, depth) {
    const { width, height, data } = depth
    const canvas = tile.media
    if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width
        canvas.height = height
    }
    let low = Infinity
    let high = -Infinity
    for (const value of data) {
        if (value > 0 && Number.isFinite(value)) {
            low = Math.min(low, value)
            high = Math.max(high, value)
        }
    }
    const span = high > low ? high - low : 1
    const context = canvas.getContext("2d")
    const pixels = context.createImageData(width, height)
    for (let index = 0; index < data.length; index++) {
        const value = data[index]
        const target = index * 4
        pixels.data[target + 3] = 255
        if (!(value > 0) || !Number.isFinite(value)) {
            continue
        }
        const shade = Math.round(((value - low) / span) * 255) * 3
        pixels.data[target] = TURBO[shade]
        pixels.data[target + 1] = TURBO[shade + 1]
        pixels.data[target + 2] = TURBO[shade + 2]
    }
    context.putImageData(pixels, 0, 0)
}

/// One connection to zenoh-web for the whole page: cameras, discovery and steering.
/// It is signalled through web_ctrl's own origin, so the page never needs the
/// bridge's address (see zenoh_web_link.rs).
const zenoh = { client: null, connecting: false, heartbeatMisses: 0, wantedMisses: 0, wantedSince: 0 }

/// Ten beats a second; the bridge fires the deadman after `deadman_ms` of silence.
const HEARTBEAT_HZ = 10
const missesFor = (deadmanMs) => Math.max(2, Math.round((deadmanMs / 1000) * HEARTBEAT_HZ))
/// A dragged deadman slider must not reconnect on every step.
const RECONNECT_SETTLE_MS = 1500

function keepZenohConnected() {
    const wanted = missesFor(state.settings.deadman_ms)
    if (wanted !== zenoh.wantedMisses) {
        zenoh.wantedMisses = wanted
        zenoh.wantedSince = performance.now()
    }
    const stale = zenoh.client && zenoh.heartbeatMisses !== wanted
        && performance.now() - zenoh.wantedSince > RECONNECT_SETTLE_MS
    if ((zenoh.client && !stale) || zenoh.connecting) {
        return
    }
    connectZenoh(wanted)
}

async function connectZenoh(heartbeatMisses) {
    zenoh.connecting = true
    const previous = zenoh.client
    zenoh.client = null
    try {
        stopDriving()
        previous?.close()
        const client = await connect(`${location.origin}/zenoh-web`, { heartbeatHz: HEARTBEAT_HZ, heartbeatMisses })
        zenoh.client = client
        zenoh.heartbeatMisses = heartbeatMisses
        for (const tile of state.tiles.values()) {
            subscribeTile(tile)
        }
        discoverCameras()
    } catch (error) {
        console.warn("zenoh-web connection failed, retrying", error)
        await new Promise((resolve) => setTimeout(resolve, 2000))
    } finally {
        zenoh.connecting = false
    }
}

const TWIST_TYPE = "geometry_msgs.Twist"
const TWIST_FINGERPRINT = [0x2e, 0x7c, 0x07, 0xd7, 0xcd, 0xf7, 0xe0, 0x27]

/// The bytes dimos's own `Twist.lcm_encode` produces: the type's fingerprint, then
/// linear and angular xyz as big-endian doubles.
function encodeTwist(linear, angular) {
    const bytes = new Uint8Array(56)
    bytes.set(TWIST_FINGERPRINT)
    const view = new DataView(bytes.buffer)
    for (const [index, value] of [...linear, ...angular].entries()) {
        view.setFloat64(8 + index * 8, value, false)
    }
    return bytes
}

const ZERO_TWIST = encodeTwist([0, 0, 0], [0, 0, 0])

/// The key dimos's zenoh transport uses for a channel: `dimos/<name>/<msg_name>`.
const dimosKey = (topic, msgType) => `dimos/${topic.replace(/^\/+/, "")}/${msgType}`

const clampUnit = (value) => Math.max(-1, Math.min(1, value))

/// The browser's stick and keys, in screen space, as a REP-103 twist: +y is left and
/// +yaw counter-clockwise, so screen-right strafes and turns negative, matching
/// dimos's own keyboard teleop.
function currentTwist(settings) {
    const turnSign = settings.invert_turn ? 1 : -1
    return {
        linear: [clampUnit(state.axes.forward) * settings.linear_speed, -clampUnit(state.axes.strafe) * settings.linear_speed, 0],
        angular: [0, 0, clampUnit(state.axes.turn) * settings.angular_speed * turnSign],
    }
}

/// Steering goes straight from this page onto zenoh, through zenoh-web; web_ctrl
/// mirrors it onto lcm. Nothing is published while nobody steers: a held control
/// publishes at `publish_hz`, a release is followed by a second of zeros so the stop
/// is heard, then the topic goes quiet so a parked browser cannot drown out other
/// teleop sources. While steering the bridge holds a zero twist as a deadman and
/// publishes it if this page goes silent for `deadman_ms`, or disconnects.
const drive = { publisher: null, client: null, key: null, armed: false, stopFlush: 0, timer: null, hz: 0 }

function keepDriving() {
    const hz = state.settings.publish_hz
    if (drive.hz === hz && drive.timer) {
        return
    }
    clearInterval(drive.timer)
    drive.hz = hz
    drive.timer = setInterval(driveTick, 1000 / hz)
}

function commandPublisher() {
    const client = zenoh.client
    if (!client || client.state === "lost") {
        return null
    }
    const key = dimosKey(state.settings.publish_topic, TWIST_TYPE)
    const current = drive.publisher
    const usable = current && !["tripped", "closed"].includes(current.state)
    if (usable && drive.key === key && drive.client === client) {
        return current
    }
    if (usable && drive.client === client) {
        // The topic was renamed mid-drive: the old one gets its stop first.
        if (drive.armed) {
            current.put(ZERO_TWIST)
        }
        current.close()
    }
    drive.publisher = client.publisher(key, { priority: Priority.REAL_TIME, latencyLimit: state.settings.deadman_ms })
    drive.client = client
    drive.key = key
    drive.armed = false
    return drive.publisher
}

function driveTick() {
    const settings = state.settings
    if (!settings) {
        return
    }
    const twist = currentTwist(settings)
    const moving = [...twist.linear, ...twist.angular].some((value) => value !== 0)
    if (moving) {
        drive.stopFlush = Math.round(settings.publish_hz)
    } else if (drive.stopFlush > 0) {
        drive.stopFlush -= 1
    } else {
        if (drive.armed) {
            disarmDeadman()
        }
        return
    }
    const publisher = commandPublisher()
    if (!publisher || publisher.state === "rejected") {
        return
    }
    try {
        if (moving && !drive.armed) {
            drive.armed = true
            publisher.setDeadman(ZERO_TWIST).catch((error) => {
                drive.armed = false
                console.warn("could not arm the deadman", error)
            })
        }
        publisher.put(encodeTwist(twist.linear, twist.angular))
    } catch {
        // Tripped since the last tick; the next one makes a fresh publisher.
        drive.armed = false
    }
}

function disarmDeadman() {
    drive.armed = false
    const publisher = drive.publisher
    if (publisher && publisher.state === "open") {
        publisher.clearDeadman().catch(() => {})
    }
}

/// Before the connection it lives on is replaced. A deadman armed on the old one
/// fires as that connection closes, which is the stop we want anyway.
function stopDriving() {
    drive.publisher?.close()
    drive.publisher = null
    drive.armed = false
}

function updateAxesFromKeys() {
    const held = (...names) => names.some((name) => state.keys.has(name))
    state.axes.forward = (held("w", "arrowup") ? 1 : 0) - (held("s", "arrowdown") ? 1 : 0)
    state.axes.turn = (held("d", "arrowright") ? 1 : 0) - (held("a", "arrowleft") ? 1 : 0)
    state.axes.strafe = (held("e") ? 1 : 0) - (held("q") ? 1 : 0)
    for (const span of document.querySelectorAll(".keys span, .dpad-key")) {
        span.classList.toggle("down", state.keys.has(span.dataset.key))
    }
}

/// The buttons feed the same held-key set as the keyboard, so a press pins one
/// axis to exactly full scale, which is what driving perfectly straight for a
/// recording needs and what a stick cannot give you.
function setupButtons() {
    for (const button of document.querySelectorAll(".dpad-key")) {
        const key = button.dataset.key
        const apply = () => {
            updateAxesFromKeys()
            renderValues()
        }
        button.addEventListener("pointerdown", (event) => {
            event.preventDefault()
            button.setPointerCapture(event.pointerId)
            if (key === "stop") {
                state.keys.clear()
            } else {
                state.keys.add(key)
            }
            apply()
        })
        for (const name of ["pointerup", "pointercancel"]) {
            button.addEventListener(name, () => {
                state.keys.delete(key)
                apply()
            })
        }
    }
}

function setupKeyboard() {
    const tracked = ["w", "a", "s", "d", "q", "e", "arrowup", "arrowdown", "arrowleft", "arrowright"]
    addEventListener("keydown", (event) => {
        const key = event.key.toLowerCase()
        if (event.target.matches("input")) {
            return
        }
        if (key === " ") {
            state.keys.clear()
        } else if (tracked.includes(key)) {
            state.keys.add(key)
        } else {
            return
        }
        event.preventDefault()
        updateAxesFromKeys()
        renderValues()
    })
    addEventListener("keyup", (event) => {
        state.keys.delete(event.key.toLowerCase())
        updateAxesFromKeys()
        renderValues()
    })
    addEventListener("blur", () => {
        state.keys.clear()
        updateAxesFromKeys()
    })
}

function setupPad() {
    const pad = element("pad")
    const knob = element("pad-knob")
    const radius = () => pad.clientWidth / 2 - knob.clientWidth / 2

    const move = (event) => {
        const bounds = pad.getBoundingClientRect()
        const limit = radius()
        let offsetX = event.clientX - bounds.left - bounds.width / 2
        let offsetY = event.clientY - bounds.top - bounds.height / 2
        const distance = Math.hypot(offsetX, offsetY)
        if (distance > limit) {
            offsetX = (offsetX / distance) * limit
            offsetY = (offsetY / distance) * limit
        }
        knob.style.transform = `translate(${offsetX}px, ${offsetY}px)`
        const sideways = offsetX / limit
        state.axes.forward = -offsetY / limit
        state.axes.turn = state.strafeMode ? 0 : sideways
        state.axes.strafe = state.strafeMode ? sideways : 0
        renderValues()
    }

    const release = () => {
        state.pad.active = false
        pad.classList.remove("active")
        knob.style.transform = "translate(0, 0)"
        state.axes.forward = 0
        state.axes.turn = 0
        state.axes.strafe = 0
        renderValues()
    }

    pad.addEventListener("pointerdown", (event) => {
        state.pad.active = true
        pad.classList.add("active")
        pad.setPointerCapture(event.pointerId)
        move(event)
    })
    pad.addEventListener("pointermove", (event) => {
        if (state.pad.active) {
            move(event)
        }
    })
    pad.addEventListener("pointerup", release)
    pad.addEventListener("pointercancel", release)
    element("strafe-mode").addEventListener("change", (event) => {
        state.strafeMode = event.target.checked
    })
}

const describeTradeoff = (value) => {
    if (value <= 0.3) {
        return "keep it sharp"
    }
    if (value >= 0.7) {
        return "keep it smooth"
    }
    return "balanced"
}

const SETTING_INPUTS = {
    "linear-speed": ["linear_speed", (value) => `${(+value).toFixed(2)} m/s`],
    "angular-speed": ["angular_speed", (value) => `${(+value).toFixed(1)} rad/s`],
    "deadman-ms": ["deadman_ms", (value) => `${(+value).toFixed(0)} ms`],
    quality: ["quality", (value) => `${value}%`],
    "max-hz": ["max_hz", (value) => (+value === 0 ? "unlimited" : `${value} Hz`)],
    tradeoff: ["quality_to_hz_tradeoff", describeTradeoff],
}

const SETTING_TOGGLES = {
    "invert-turn": "invert_turn",
    "auto-quality": "auto_quality",
}

const RECORD_SELECTS = {
    "record-image-format": "record_image_format",
    "record-compression": "record_compression",
}

function renderSettings(settings) {
    const topic = element("publish-topic")
    if (document.activeElement !== topic) {
        topic.value = settings.publish_topic
    }
    for (const [id, [key, format]] of Object.entries(SETTING_INPUTS)) {
        const input = element(id)
        input.value = settings[key]
        setText(element(`label-${id}`), format(input.value))
    }
    for (const [id, key] of Object.entries(SETTING_TOGGLES)) {
        element(id).checked = settings[key]
    }

    // The writer is built when recording starts, so a mid-run change would
    // silently not apply.
    const recording = state.recording?.active === true
    for (const [id, key] of Object.entries(RECORD_SELECTS)) {
        const select = element(id)
        select.value = settings[key]
        select.disabled = recording
    }
    const directory = element("record-dir")
    if (document.activeElement !== directory) {
        directory.value = settings.record_dir
    }
    directory.disabled = recording
}

function setupSettings() {
    // Sent on commit rather than per keystroke, so a half-typed name never
    // becomes the topic the robot is being driven on.
    const topicInput = element("publish-topic")
    topicInput.addEventListener("change", (event) => {
        sendSetting("publish_topic", event.target.value)
    })
    topicInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
            event.target.blur()
        }
    })
    for (const [id, [key]] of Object.entries(SETTING_INPUTS)) {
        element(id).addEventListener("input", (event) => {
            sendSetting(key, Number(event.target.value))
            renderSettings(state.settings)
        })
    }
    for (const [id, key] of Object.entries(SETTING_TOGGLES)) {
        element(id).addEventListener("change", (event) => {
            sendSetting(key, event.target.checked)
            renderSettings(state.settings)
        })
    }
    for (const [id, key] of Object.entries(RECORD_SELECTS)) {
        element(id).addEventListener("change", (event) => {
            sendSetting(key, event.target.value)
        })
    }
    const directory = element("record-dir")
    directory.addEventListener("change", (event) => {
        sendSetting("record_dir", event.target.value)
    })
    directory.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
            event.target.blur()
        }
    })
    const show = (open) => {
        element("settings").hidden = !open
        element("settings-scrim").hidden = !open
    }
    element("settings-button").addEventListener("click", () => show(true))
    element("settings-close").addEventListener("click", () => show(false))
    element("settings-scrim").addEventListener("click", () => show(false))

    const showTf = (open) => {
        element("tf-panel").hidden = !open
        element("tf-scrim").hidden = !open
    }
    element("tf-open").addEventListener("click", () => showTf(true))
    element("tf-close").addEventListener("click", () => showTf(false))
    element("tf-scrim").addEventListener("click", () => showTf(false))

    const showRecord = (open) => {
        element("record-panel").hidden = !open
        element("record-scrim").hidden = !open
        if (open) {
            pollRecordings()
        }
    }
    element("record-open").addEventListener("click", () => showRecord(true))
    element("record-close").addEventListener("click", () => showRecord(false))
    element("record-scrim").addEventListener("click", () => showRecord(false))

    element("record-toggle").addEventListener("click", () => {
        send({ type: state.recording?.active ? "stop_record" : "record" })
    })

    const showLaunch = (open) => {
        element("launch-panel").hidden = !open
        element("launch-scrim").hidden = !open
    }
    element("launch-open").addEventListener("click", () => showLaunch(true))
    element("launch-close").addEventListener("click", () => showLaunch(false))
    element("launch-scrim").addEventListener("click", () => showLaunch(false))

    const copyButton = element("launch-copy")
    copyButton.addEventListener("click", async () => {
        await copyText(element("launch-output").textContent)
        copyButton.textContent = "Copied"
        setTimeout(() => { copyButton.textContent = "Copy" }, 1200)
    })

    element("launch-kill").addEventListener("click", () => {
        send({ type: "launch_kill" })
        // The sweep takes a second or two before anything reaches the output, which
        // without this reads as the button having done nothing at all.
        killPressedAt = performance.now()
        renderKillButton()
    })

    const nameInput = element("launch-name")
    const commandInput = element("launch-command")
    const saveButton = element("launch-save")
    const refreshSaveButton = () => {
        saveButton.disabled = !nameInput.value.trim() || !commandInput.value.trim()
    }
    const saveCommand = () => {
        const name = nameInput.value.trim()
        const command = commandInput.value.trim()
        if (!name || !command) {
            return
        }
        send({ type: "launch_save", name, command })
        nameInput.value = ""
        commandInput.value = ""
        refreshSaveButton()
        if (launcherView) {
            const existing = launcherView.commands.some((saved) => saved.name === name)
            renderLauncher({
                ...launcherView,
                commands: existing
                    ? launcherView.commands.map((saved) => saved.name === name ? { name, command } : saved)
                    : [...launcherView.commands, { name, command }],
            })
        }
    }
    for (const input of [nameInput, commandInput]) {
        input.addEventListener("input", refreshSaveButton)
        input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
                saveCommand()
            }
        })
    }
    saveButton.addEventListener("click", saveCommand)
}

// A hidden tab keeps a stale command alive on some phones; drop the stick instead.
document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
        state.keys.clear()
        state.axes = { forward: 0, strafe: 0, turn: 0 }
        renderValues()
    }
})

setupKeyboard()
setupPad()
setupButtons()
setupSettings()
connectControl()
pollTf()
setInterval(pollTf, 2000)
setInterval(discoverCameras, 2000)
