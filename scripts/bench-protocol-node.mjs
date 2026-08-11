#!/usr/bin/env node
/**
 * Benchmarks the JS protocol implementation: encode + incremental decode of a
 * realistic session_progress event, measuring wall time and heap usage.
 *
 * Run from the repo root: node scripts/bench-protocol-node.mjs [--iterations N]
 * Prints a JSON report to stdout.
 */
import { performance } from "node:perf_hooks";
import { encodeServerMessage, ServerMessageDecoder } from "../packages/protocol/src/index.ts";

function parseIterations(argv) {
	const flag = argv.indexOf("--iterations");
	if (flag === -1) return 10_000;
	const raw = argv[flag + 1];
	const n = Number(raw);
	if (!Number.isInteger(n) || n < 0) {
		console.error(`Invalid --iterations value: ${raw}`);
		process.exit(2);
	}
	return n;
}

const iterations = parseIterations(process.argv.slice(2));

// Realistic session_progress events matching the wire shapes used in
// packages/protocol/test/protocol.test.ts. Plain objects only, so TypeBox
// validation accepts them.
const toolMessage = {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: {
				id: "tool-1",
				role: "tool",
				toolCallId: "call-1",
				toolName: "read",
				input: { path: "/tmp/file", lines: [1, 2, 3] },
				content: [
					{ type: "text", text: "line 1\nline 2\nline 3\n" },
					{ type: "text", text: "(3 lines)" },
				],
				details: { cached: false, size: 1234 },
				status: "complete",
				isError: false,
				timestamp: 1,
			},
		},
	},
};

const assistantMessage = {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: {
				id: "assistant-1",
				role: "assistant",
				content: [{ type: "text", text: "Here is the summary." }],
				model: { provider: "test", id: "model-1" },
				responseModel: "test-model",
				usage: {
					input: 42,
					output: 7,
					cacheRead: 0,
					cacheWrite: 0,
					totalTokens: 49,
					cost: { input: 0.021, output: 0.028, cacheRead: 0, cacheWrite: 0, total: 0.049 },
				},
				timestamp: 2,
				status: "complete",
				stopReason: "stop",
			},
		},
	},
};

const messages = [toolMessage, assistantMessage];
const heap = () => process.memoryUsage().heapUsed;

// Warmup: JIT the encode/decode paths before measuring.
const warmup = Math.min(1_000, iterations);
for (let i = 0; i < warmup; i++) {
	encodeServerMessage(messages[i % 2]);
}

const baselineHeap = heap();
const decoder = new ServerMessageDecoder();
const start = performance.now();
let peakHeap = baselineHeap;
let heapSamples = 0;
let decodedMessages = 0;
for (let i = 0; i < iterations; i++) {
	decoder.push(encodeServerMessage(messages[i % 2]));
	decodedMessages++;
	if ((i & 63) === 0) {
		const current = heap();
		if (current > peakHeap) peakHeap = current;
		heapSamples++;
	}
}
decoder.end();
const elapsedMs = performance.now() - start;
const finalHeap = heap();
const rss = process.memoryUsage().rss;

if (decodedMessages !== iterations) {
	console.error(`Expected ${iterations} decoded messages, got ${decodedMessages}`);
	process.exit(1);
}

const report = {
	package: "@earendil-works/pi-protocol",
	runtime: `node ${process.version}`,
	iterations,
	messagesEncoded: iterations,
	messagesDecoded: decodedMessages,
	elapsedMs: Math.round(elapsedMs * 100) / 100,
	messagesPerSecond: Math.round((iterations / elapsedMs) * 1000),
	heapUsedBaselineBytes: baselineHeap,
	heapUsedPeakBytes: peakHeap,
	heapUsedFinalBytes: finalHeap,
	heapDeltaBytes: finalHeap - baselineHeap,
	heapSamples,
	rssBytes: rss,
};
console.log(JSON.stringify(report, null, 2));
