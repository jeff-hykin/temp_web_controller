#!/usr/bin/env -S deno run --allow-read
// Markdown table of measure.js results: deno run --allow-read test/r1/summarize.js <dir with *.json>...

const f = (value, digits = 1) => (value === null || value === undefined || Number.isNaN(value) ? "-" : value.toFixed(digits))

const rows = []
for (const dir of Deno.args) {
    for (const entry of [...Deno.readDirSync(dir)].filter((entry) => entry.name.endsWith(".json")).sort((a, b) => a.name.localeCompare(b.name))) {
        const result = JSON.parse(Deno.readTextFileSync(`${dir}/${entry.name}`))
        for (const [index, tab] of result.tabs.entries()) {
            for (const stream of tab.streams) {
                const isMain = result.version === "main"
                const bytes = isMain ? stream.bytesPerFrame : stream.tab.videoPayloadBytesPerFrame
                const mbps = isMain ? stream.mbps : stream.tab.videoPayloadMbps
                const size = isMain ? (tab.tileBars.find((bar) => bar.includes(stream.camera)) ?? "").match(/(\d+x\d+ q\d+)/)?.[1] ?? "" : `${stream.videoSize} q${f(stream.quality.p50, 2)}`
                // an unmarked camera has no exact latency; zenoh-web's metadata pairing estimates it (marked ~)
                const estimated = stream.latencyShownMs.n === 0 && stream.metadataPairing?.latencyShownMs.n > 0
                const shown = estimated ? stream.metadataPairing.latencyShownMs : stream.latencyShownMs
                const mark = estimated ? "~" : ""
                rows.push(`| ${result.label} | ${result.tabs.length > 1 ? `tab ${index + 1} ` : ""}${stream.camera} | ${f(stream.shownFps)} / ${f(stream.sourceFps, 0)} | ${mark}${f(shown.p50, 0)} | ${mark}${f(shown.p95, 0)} | ${f(stream.latencyArrivalMs.p50, 0)} | ${f(bytes / 1024)} | ${f(mbps, 2)} | ${f(result.cpu.mean, 0)} | ${size} | ${f(100 * stream.matchedFraction, 0)}% | ±${f(result.clock.used.errorMs, 1)} |`)
            }
        }
    }
}
console.log("| run | stream | shown fps / source fps | latency p50 ms | latency p95 ms | arrival p50 ms | KB/frame | Mbps | R1 CPU % | size (end of run) | frames matched | clock ± ms |")
console.log("|---|---|---|---|---|---|---|---|---|---|---|---|")
console.log(rows.join("\n"))
