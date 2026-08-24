const element = (id) => document.getElementById(id)

const state = {
    settings: null,
    watching: new Set(),
    tiles: new Map(),
    axes: { forward: 0, strafe: 0, turn: 0 },
    keys: new Set(),
    pad: { active: false, x: 0, y: 0 },
    strafeMode: false,
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
    element("publish-target").textContent = transports
        ? `driving ${publish.topic} over ${transports}`
        : "not publishing"

    if (!state.settings || !state.editingSettings) {
        state.settings = status.settings
        renderSettings(status.settings)
    }
    renderCameras(status.topics)
    renderTopics(status.topics)
    renderTileStats(status.streams)
    renderValues()
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
            picker.append(chip)
        }
        chip.textContent = `${topic.topic} · ${topic.rate.toFixed(0)}hz`
        chip.classList.toggle("on", state.watching.has(topic.topic))
    }
    if (images.length && state.watching.size === 0) {
        toggleStream(images[0].topic)
    }
    element("streams-empty").hidden = state.watching.size > 0
}

function renderTopics(topics) {
    const table = element("topic-table")
    table.replaceChildren(...topics.map((topic) => {
        const row = document.createElement("div")
        row.className = topic.seconds_since_seen > 5 ? "topic-row stale" : "topic-row"
        const name = document.createElement("span")
        name.textContent = topic.topic
        const type = document.createElement("span")
        type.className = "type"
        type.textContent = topic.msg_type ?? "?"
        const rate = document.createElement("span")
        rate.textContent = `${topic.rate.toFixed(0)} hz`
        row.append(name, type, rate)
        return row
    }))
}

function renderTileStats(streams) {
    for (const [topic, tile] of state.tiles) {
        const stats = streams[topic]
        if (!stats) {
            continue
        }
        const size = stats.passthrough ? "jpeg" : `${stats.width}x${stats.height} q${stats.quality}`
        const kilobytes = (stats.jpeg_bytes / 1024).toFixed(0)
        tile.info.textContent = stats.error
            ? stats.error
            : `${stats.stream_fps.toFixed(0)}/${stats.source_fps.toFixed(0)} fps · ${size} · ${kilobytes} KB`
    }
}

function renderTf(view) {
    const warnings = element("tf-warnings")
    warnings.hidden = view.warnings.length === 0
    warnings.replaceChildren(...view.warnings.map((text) => {
        const line = document.createElement("span")
        line.textContent = text
        return line
    }))
    element("settings-badge").hidden = view.warnings.length === 0

    const childrenOf = new Map()
    for (const link of view.links) {
        if (!childrenOf.has(link.parent)) {
            childrenOf.set(link.parent, [])
        }
        childrenOf.get(link.parent).push(link)
    }

    const tree = element("tf-tree")
    if (view.links.length === 0) {
        tree.textContent = "no tf seen yet"
        return
    }

    const lines = []
    const drawn = new Set()
    const walk = (frame, depth, link) => {
        const row = document.createElement("div")
        if (link && link.stale) {
            row.className = "stale"
        }
        const name = document.createElement("span")
        name.className = "frame"
        name.textContent = `${"  ".repeat(depth)}${depth ? "└ " : ""}${frame}`
        row.append(name)
        if (link) {
            const tag = document.createElement("span")
            tag.className = "tag"
            tag.textContent = link.is_static
                ? "  static"
                : `  ${link.seconds_since_seen.toFixed(0)}s ago`
            row.append(tag)
        }
        lines.push(row)
        // A double parent or a cycle would otherwise recurse forever.
        if (drawn.has(frame)) {
            return
        }
        drawn.add(frame)
        for (const child of childrenOf.get(frame) ?? []) {
            walk(child.child, depth + 1, child)
        }
    }
    for (const root of view.roots) {
        walk(root, 0, null)
    }
    for (const link of view.links) {
        if (!drawn.has(link.child)) {
            walk(link.child, 0, link)
        }
    }
    tree.replaceChildren(...lines)
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
    for (const span of document.querySelectorAll(".keys span")) {
        span.classList.toggle("down", state.keys.has(span.dataset.key))
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

function renderSettings(settings) {
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
}

function setupSettings() {
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
    const show = (open) => {
        element("settings").hidden = !open
        element("settings-scrim").hidden = !open
    }
    element("settings-button").addEventListener("click", () => show(true))
    element("settings-close").addEventListener("click", () => show(false))
    element("settings-scrim").addEventListener("click", () => show(false))
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
setupSettings()
connectControl()
startCommandLoop()
pollTf()
setInterval(pollTf, 2000)
