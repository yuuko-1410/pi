// Generates byte-level differential fixtures for the Rust protocol port.
//
// The fixtures are encoded by the real JS implementation
// (packages/protocol) and consumed by rust/pi-protocol/tests/differential.rs,
// which decodes each fixture, re-encodes it, and asserts byte identity.
//
// Field order matters: JS object literal key order becomes CBOR map order,
// and the Rust `to_value` mirrors the schema declaration order. Every object
// below is therefore written in schema declaration order (schemas.ts).
//
// Run from the repo root: node scripts/generate-protocol-fixtures.mjs

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import {
	encodeClientMessage,
	encodeServerMessage,
	PROTOCOL_VERSION,
} from "../packages/protocol/src/index.ts";

const OUTPUT = fileURLToPath(new URL("../rust/pi-protocol/tests/fixtures.tsv", import.meta.url));

const toHex = (bytes) => Buffer.from(bytes).toString("hex");
const lines = [];
const add = (kind, message) => {
	// encodeClientMessage/encodeServerMessage return framed bytes (4-byte
	// length prefix + CBOR). The differential test decodes each line as raw
	// CBOR (framing has its own dedicated test file), so drop the prefix.
	const framed = encodeClientOrServer(kind, message);
	const payload = framed.subarray(4);
	lines.push(`${kind}\t${toHex(payload)}`);
};

function encodeClientOrServer(kind, message) {
	return kind === "client" ? encodeClientMessage(message) : encodeServerMessage(message);
}

// ---------------------------------------------------------------------------
// Shared building blocks (schema declaration order throughout)
// ---------------------------------------------------------------------------

const modelRef = (provider = "test-provider", id = "model-1") => ({ provider, id });

const modelCost = {
	input: 0.5,
	output: 1.5,
	cacheRead: 0.1,
	cacheWrite: 0.2,
};

const modelMetadata = {
	provider: "test-provider",
	id: "model-1",
	name: "Test Model",
	api: "test-api",
	reasoning: true,
	input: ["text", "image"],
	contextWindow: 128000,
	maxTokens: 16384,
	cost: modelCost,
	supportedThinkingLevels: ["off", "low", "medium", "high", "max"],
	authenticated: true,
};

const usage = {
	input: 10,
	output: 20,
	cacheRead: 5,
	cacheWrite: 3,
	reasoning: 7,
	totalTokens: 45,
	cost: { input: 0.001, output: 0.002, cacheRead: 0.0001, cacheWrite: 0.0002, total: 0.0033 },
};

const textContent = (text = "hello") => ({ type: "text", text });
const thinkingContent = (thinking = "hmm", redacted = true) => ({ type: "thinking", thinking, redacted });
const imageContent = (data = "aGVsbG8=", mimeType = "image/png") => ({ type: "image", data, mimeType });
const toolCallContent = (toolCallId = "call-1", toolName = "read", input = { path: "/tmp/file" }) => ({
	type: "toolCall",
	toolCallId,
	toolName,
	input,
});

const userItem = (id = "u-1", content = [textContent()], timestamp = 1) => ({
	id,
	role: "user",
	content,
	timestamp,
});

const assistantItem = ({
	id = "a-1",
	content = [textContent()],
	model = modelRef(),
	responseModel = undefined,
	usage: itemUsage = undefined,
	timestamp = 2,
	...status
}) => ({
	id,
	role: "assistant",
	content,
	model,
	...(responseModel !== undefined ? { responseModel } : {}),
	...(itemUsage !== undefined ? { usage: itemUsage } : {}),
	timestamp,
	...status,
});

const toolItem = ({
	id = "t-1",
	toolCallId = "call-1",
	toolName = "read",
	input = { path: "/tmp/file" },
	content = [textContent("done")],
	details = undefined,
	usage: itemUsage = undefined,
	timestamp = 3,
	...status
}) => ({
	id,
	role: "tool",
	toolCallId,
	toolName,
	input,
	content,
	...(details !== undefined ? { details } : {}),
	...(itemUsage !== undefined ? { usage: itemUsage } : {}),
	timestamp,
	...status,
});

const sessionMetadata = (id = "session-1") => ({
	id,
	createdAt: 1,
	updatedAt: 2,
	parentSessionId: "parent-1",
	sessionName: "Named session",
	cwd: "/workspace",
});

const transcript = [
	userItem("u-1", [textContent("hi"), imageContent()], 1),
	assistantItem({
		id: "a-1",
		content: [thinkingContent("let me think", true), toolCallContent()],
		usage,
		timestamp: 2,
		status: "complete",
		stopReason: "toolUse",
	}),
	toolItem({
		id: "t-1",
		toolCallId: "call-1",
		toolName: "read",
		input: { path: "/tmp/file" },
		content: [textContent("done")],
		details: { lines: [1, 2, 3], cached: false },
		usage,
		timestamp: 3,
		status: "complete",
		isError: false,
	}),
	toolItem({
		id: "t-2",
		toolCallId: "call-2",
		toolName: "write",
		input: { path: "/tmp/out", content: "x" },
		content: [],
		timestamp: 4,
		status: "error",
		isError: true,
	}),
	assistantItem({
		id: "a-2",
		content: [textContent("second turn")],
		timestamp: 5,
		status: "aborted",
		stopReason: "aborted",
		errorMessage: "interrupted",
	}),
];

const fullSessionSnapshot = {
	id: "session-1",
	name: "Named session",
	cwd: "/workspace",
	createdAt: 1,
	updatedAt: 2,
	phase: "turn",
	model: modelRef(),
	thinkingLevel: "high",
	attached: true,
	locked: false,
	revision: 3,
	transcript,
	queuedSteer: [userItem("steer-1", [textContent("steer message")], 10)],
	queuedSteerCount: 1,
};

const fullServerSnapshot = {
	serverId: "server-1",
	protocolVersion: PROTOCOL_VERSION,
	revision: 7,
	sessions: [sessionMetadata("session-1"), sessionMetadata("session-2")],
	models: [modelMetadata],
};

const protocolError = (code, message = "Something failed", details = undefined) => ({
	code,
	message,
	...(details !== undefined ? { details } : {}),
});

// ---------------------------------------------------------------------------
// Client fixtures
// ---------------------------------------------------------------------------

add("client", { type: "hello", version: 0 });
add("client", { type: "hello", version: PROTOCOL_VERSION });
add("client", { type: "hello", version: PROTOCOL_VERSION + 1 });

add("client", { type: "request", id: "request-1", request: { command: "list" } });
add("client", {
	type: "request",
	id: "request-2",
	request: {
		command: "create",
		cwd: "/tmp",
		name: "new session",
		model: modelRef("anthropic", "claude-sonnet"),
		thinkingLevel: "high",
	},
});
add("client", {
	type: "request",
	id: "request-3",
	request: { command: "create", cwd: "/tmp" },
});
add("client", { type: "request", id: "request-4", request: { command: "attach", sessionId: "session-1" } });
add("client", { type: "request", id: "request-5", request: { command: "detach", sessionId: "session-1" } });
add("client", {
	type: "request",
	id: "request-6",
	request: { command: "prompt", sessionId: "session-1", text: "inspect the code" },
});
add("client", {
	type: "request",
	id: "request-7",
	request: { command: "steer", sessionId: "session-1", text: "stay on task" },
});
add("client", { type: "request", id: "request-8", request: { command: "abort", sessionId: "session-1" } });
add("client", {
	type: "request",
	id: "request-9",
	request: { command: "set_model", sessionId: "session-1", model: modelRef("anthropic", "claude-opus") },
});
add("client", {
	type: "request",
	id: "request-10",
	request: { command: "set_thinking", sessionId: "session-1", thinkingLevel: "max" },
});

// ---------------------------------------------------------------------------
// Server fixtures
// ---------------------------------------------------------------------------

add("server", {
	type: "hello",
	version: PROTOCOL_VERSION,
	connectionId: "connection-1",
	snapshot: fullServerSnapshot,
});

add("server", {
	type: "hello_error",
	error: protocolError("version", "Unsupported protocol version", { supported: [PROTOCOL_VERSION] }),
});

add("server", {
	type: "response",
	id: "request-1",
	ok: true,
	result: { command: "list", sessions: [sessionMetadata("session-1"), sessionMetadata("session-2")] },
});

add("server", {
	type: "response",
	id: "request-2",
	ok: true,
	result: { command: "create", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-4",
	ok: true,
	result: { command: "attach", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-6",
	ok: true,
	result: { command: "prompt", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-7",
	ok: true,
	result: { command: "steer", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-8",
	ok: true,
	result: { command: "abort", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-9",
	ok: true,
	result: { command: "set_model", session: fullSessionSnapshot },
});
add("server", {
	type: "response",
	id: "request-10",
	ok: true,
	result: { command: "set_thinking", session: fullSessionSnapshot },
});
add("server", { type: "response", id: "request-5", ok: true, result: { command: "detach", sessionId: "session-1" } });

for (const code of [
	"version",
	"busy",
	"session_locked",
	"not_found",
	"invalid_request",
	"not_implemented",
	"internal_error",
]) {
	const details = code === "invalid_request" ? { field: "request.text", nested: { ok: true } } : undefined;
	add("server", {
		type: "response",
		id: "request-err",
		ok: false,
		error: protocolError(code, `Error with code ${code}`, details),
	});
}

add("server", { type: "event", event: { type: "server_snapshot", snapshot: fullServerSnapshot } });
add("server", { type: "event", event: { type: "session_snapshot", snapshot: fullSessionSnapshot } });
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: { type: "item_started", item: userItem() },
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_started",
			item: assistantItem({ status: "streaming" }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_started",
			item: toolItem({ status: "running", isError: false }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: { type: "assistant_delta", messageId: "a-1", contentIndex: 0, kind: "text", delta: "hel" },
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_updated",
			item: assistantItem({ id: "a-1", content: [textContent("streaming text")], status: "streaming" }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_updated",
			item: toolItem({ id: "t-1", status: "running", isError: false }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: assistantItem({ id: "a-1", status: "complete", stopReason: "stop" }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: assistantItem({
				id: "a-1",
				status: "error",
				stopReason: "error",
				errorMessage: "model failed",
			}),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: assistantItem({ id: "a-1", status: "aborted", stopReason: "aborted" }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: toolItem({ id: "t-1", status: "complete", isError: false }),
		},
	},
});
add("server", {
	type: "event",
	event: {
		type: "session_progress",
		sessionId: "session-1",
		progress: {
			type: "item_finished",
			item: toolItem({ id: "t-1", status: "error", isError: true }),
		},
	},
});
add("server", { type: "event", event: { type: "session_removed", sessionId: "session-1" } });

// ---------------------------------------------------------------------------
// Write output
// ---------------------------------------------------------------------------

mkdirSync(dirname(OUTPUT), { recursive: true });
writeFileSync(OUTPUT, lines.join("\n") + "\n");

const clientCount = lines.filter((line) => line.startsWith("client\t")).length;
const serverCount = lines.filter((line) => line.startsWith("server\t")).length;
console.log(`wrote ${OUTPUT}`);
console.log(`fixtures: ${lines.length} total (client: ${clientCount}, server: ${serverCount})`);
