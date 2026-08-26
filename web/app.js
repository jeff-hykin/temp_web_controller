const element = (id) => document.getElementById(id)

const state = {
    settings: null,
    watching: new Set(),
    tiles: new Map(),
    axes: { forward: 0, strafe: 0, turn: 0 },
    keys: new Set(),
    pad: { active: false, x: 0, y: 0 },
    strafeMode: false,
    topics: [],
    lcmError: null,
}

let control = null

function connectControl() {
    const protocol = location.protocol === "https:" ? "wss:" : "ws:"
    control = new WebSocket(`${protocol}//${location.host}/ws`)
    control.addEventListener("open", () => element("link-dot").classList.add("live"))
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
    }
}

function applyStatus(status) {
    if (status.type !== "status") {
        return
    }
    const publish = status.publish
    const transports = [publish.lcm && "lcm", publish.zenoh && "zenoh"].filter(Boolean).join(" + ")
    const target = element("publish-target")
    target.textContent = transports
        ? `driving ${publish.topic} over ${transports}`
        : "not publishing"

    const lcmError = status.receivers?.lcm_error
    state.lcmError = lcmError
    target.classList.toggle("bad", Boolean(lcmError))
    if (lcmError) {
        target.textContent += ` — not hearing lcm: ${lcmError}`
    }

    if (!state.settings || !state.editingSettings) {
        state.settings = status.settings
        renderSettings(status.settings)
    }
    renderCameras(status.topics)
    renderTopics(status.topics)
    renderTileStats(status.streams)
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

function renderCameras(topics) {
    const picker = element("camera-picker")
    const images = topics.filter((topic) => topic.is_image)
    const wanted = new Set(images.map((topic) => topic.topic))

    for (const chip of [...picker.children]) {
        if (!wanted.has(chip.dataset.topic)) {
            chip.remove()
        }
    }
    for (const topic of images) {
        let chip = picker.querySelector(`[data-topic="${CSS.escape(topic.topic)}"]`)
        if (!chip) {
            chip = document.createElement("button")
            chip.className = "chip"
            chip.dataset.topic = topic.topic
            chip.addEventListener("click", () => toggleStream(topic.topic))
            // The rate lives in its own fixed-width span: written inline, every
            // 9→10 hz tick resized the chip and shoved its neighbours sideways.
            chip.append(
                Object.assign(document.createElement("span"), { textContent: `${topic.topic} · ` }),
                Object.assign(document.createElement("span"), { className: "hz" }),
            )
            picker.append(chip)
        }
        setText(chip.lastChild, `${topic.rate.toFixed(0)}hz`)
        chip.classList.toggle("on", state.watching.has(topic.topic))
    }
    if (images.length && state.watching.size === 0) {
        toggleStream(images[0].topic)
    }
    element("streams-empty").hidden = state.watching.size > 0
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
            setText(type, topic.msg_type ?? "?")
            setText(rate, `${topic.rate.toFixed(0)} hz`)
        }
        return row
    })
}

function renderTileStats(streams) {
    for (const [topic, tile] of state.tiles) {
        const stats = streams[topic]
        if (!stats) {
            continue
        }
        // Quality is ours to report only when we did the encoding; a passed-through
        // frame carries whatever the publisher chose.
        const size = stats.passthrough
            ? `${stats.width}x${stats.height} as sent`
            : `${stats.width}x${stats.height} q${stats.quality}`
        const kilobytes = (stats.jpeg_bytes / 1024).toFixed(0)
        setText(tile.info, stats.error
            ? stats.error
            : `${stats.stream_fps.toFixed(0)}/${stats.source_fps.toFixed(0)} fps · ${size} · ${kilobytes} KB`)
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

function toggleStream(topic) {
    if (state.watching.has(topic)) {
        state.watching.delete(topic)
        const tile = state.tiles.get(topic)
        if (tile) {
            tile.socket.close()
            tile.root.remove()
            state.tiles.delete(topic)
        }
    } else {
        state.watching.add(topic)
        openStream(topic)
    }
    element("streams-empty").hidden = state.watching.size > 0
}

function openStream(topic) {
    const root = document.createElement("div")
    root.className = "tile"
    const canvas = document.createElement("canvas")
    const bar = document.createElement("div")
    bar.className = "tile-bar"
    const name = document.createElement("strong")
    name.textContent = topic
    const info = document.createElement("span")
    info.textContent = "connecting"
    bar.append(name, info)
    root.append(canvas, bar)
    element("streams").append(root)

    const context = canvas.getContext("2d")
    const protocol = location.protocol === "https:" ? "wss:" : "ws:"
    const socket = new WebSocket(`${protocol}//${location.host}/ws/stream/${topic}`)
    socket.binaryType = "arraybuffer"

    let decoding = false
    socket.addEventListener("message", async (event) => {
        // Skip arriving frames while one is still decoding: latest wins, always.
        if (decoding || typeof event.data === "string") {
            return
        }
        decoding = true
        try {
            const bitmap = await createImageBitmap(new Blob([event.data], { type: "image/jpeg" }))
            if (canvas.width !== bitmap.width || canvas.height !== bitmap.height) {
                canvas.width = bitmap.width
                canvas.height = bitmap.height
            }
            context.drawImage(bitmap, 0, 0)
            bitmap.close()
        } catch (error) {
            info.textContent = `decode failed: ${error}`
        }
        decoding = false
    })
    socket.addEventListener("close", () => {
        if (state.watching.has(topic)) {
            info.textContent = "disconnected"
        }
    })

    state.tiles.set(topic, { root, canvas, info, socket })
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

const SETTING_INPUTS = {
    "linear-speed": ["linear_speed", (value) => `${(+value).toFixed(2)} m/s`],
    "angular-speed": ["angular_speed", (value) => `${(+value).toFixed(1)} rad/s`],
    "deadman-ms": ["deadman_ms", (value) => `${(+value).toFixed(0)} ms`],
    quality: ["quality", (value) => `${value}`],
    "max-width": ["max_width", (value) => `${value} px`],
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
        if (document.activeElement !== input) {
            input.value = settings[key]
        }
        element(`label-${id}`).textContent = format(input.value)
    }
    for (const [id, key] of Object.entries(SETTING_TOGGLES)) {
        element(id).checked = settings[key]
    }
    element("quality").disabled = settings.auto_quality

    for (const [id, key] of Object.entries(RECORD_SELECTS)) {
        const select = element(id)
        select.value = settings[key]
        // The writer is built when recording starts, so a mid-run change would
        // silently not apply.
        select.disabled = state.recording?.active === true
    }
}

function setupSettings() {
    // Sent on commit rather than per keystroke, so a half-typed name never
    // becomes the topic the robot is being driven on.
    const topicInput = element("publish-topic")
    topicInput.addEventListener("change", (event) => {
        send({ type: "settings", publish_topic: event.target.value })
    })
    topicInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
            event.target.blur()
        }
    })
    for (const [id, [key]] of Object.entries(SETTING_INPUTS)) {
        element(id).addEventListener("input", (event) => {
            const value = Number(event.target.value)
            state.settings[key] = value
            state.editingSettings = true
            renderSettings(state.settings)
            send({ type: "settings", [key]: value })
        })
        element(id).addEventListener("change", () => {
            state.editingSettings = false
        })
    }
    for (const [id, key] of Object.entries(SETTING_TOGGLES)) {
        element(id).addEventListener("change", (event) => {
            state.settings[key] = event.target.checked
            renderSettings(state.settings)
            send({ type: "settings", [key]: event.target.checked })
        })
    }
    for (const [id, key] of Object.entries(RECORD_SELECTS)) {
        element(id).addEventListener("change", (event) => {
            state.settings[key] = event.target.value
            send({ type: "settings", [key]: event.target.value })
        })
    }
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

function startCommandLoop() {
    setInterval(() => {
        send({
            type: "cmd",
            forward: state.axes.forward,
            strafe: state.axes.strafe,
            turn: state.axes.turn,
        })
    }, 50)
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
startCommandLoop()
pollTf()
setInterval(pollTf, 2000)
