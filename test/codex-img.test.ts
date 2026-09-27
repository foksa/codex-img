import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { extractChatGptAccountId, loadCredentials, tokenExpiresSoon } from "../src/auth.js";
import { formatProblem, outputPath, parseCli } from "../src/cli.js";
import { CodexImageError, parseCodexSse } from "../src/codex-response.js";
import { buildRequestBody, decodeImageData, generateImage } from "../src/generate.js";

// 1x1 transparent PNG
const PNG_B64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

function jwt(payload: Record<string, unknown>): string {
	const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
	return `${part({ alg: "none" })}.${part(payload)}.sig`;
}

function token(expInSeconds: number, accountId = "acct_123"): string {
	return jwt({ exp: Math.floor(Date.now() / 1000) + expInSeconds, "https://api.openai.com/auth": { chatgpt_account_id: accountId } });
}

function sse(events: unknown[]): Response {
	const body = events.map(event => `data: ${JSON.stringify(event)}\n\n`).join("");
	return new Response(body, { headers: { "content-type": "text/event-stream" } });
}

const imageEvents = [
	{ type: "response.created", response: { id: "resp_1" } },
	{ type: "response.image_generation_call.generating" },
	{
		type: "response.output_item.done",
		item: { type: "image_generation_call", id: "ig_1", status: "completed", result: PNG_B64, revised_prompt: "a fox", size: "1024x1024", quality: "high" },
	},
	{ type: "response.completed", response: { id: "resp_1", tools: [{ type: "image_generation", model: "gpt-image-2-codex" }], usage: { input_tokens: 10, output_tokens: 2, total_tokens: 12 } } },
];

describe("parseCodexSse", () => {
	test("extracts the image, metadata, usage and progress", async () => {
		const stages: string[] = [];
		const parsed = await parseCodexSse(sse(imageEvents), new AbortController().signal, [], stage => stages.push(stage));
		expect(parsed.image?.id).toBe("ig_1");
		expect(parsed.image?.result).toBe(PNG_B64);
		expect(parsed.image?.revisedPrompt).toBe("a fox");
		expect(parsed.image?.reported).toEqual({ model: "gpt-image-2-codex", size: "1024x1024", quality: "high" });
		expect(parsed.responseId).toBe("resp_1");
		expect(parsed.usage).toEqual({ input_tokens: 10, output_tokens: 2, total_tokens: 12 });
		expect(stages).toEqual(["generating"]);
	});

	test("classifies quota failures", async () => {
		const failed = sse([{ type: "response.failed", response: { error: { code: "usage_limit_reached" } } }]);
		const error = await parseCodexSse(failed, new AbortController().signal, []).catch(e => e);
		expect(error).toBeInstanceOf(CodexImageError);
		expect(error.kind).toBe("quota");
	});

	test("rejects a stream that never completes", async () => {
		await expect(parseCodexSse(sse(imageEvents.slice(0, 3)), new AbortController().signal, [])).rejects.toThrow("ended before completion");
	});
});

describe("images", () => {
	test("decodeImageData validates base64 and magic bytes", () => {
		const decoded = decodeImageData(PNG_B64);
		expect(decoded.format).toBe("png");
		expect(decoded.bytes.length).toBeGreaterThan(8);
		expect(() => decodeImageData(Buffer.from("GIF89a-not-supported").toString("base64"))).toThrow("not PNG, JPEG or WebP");
		expect(() => decodeImageData("not base64!")).toThrow("invalid base64");
	});

	test("buildRequestBody only sends optional tool fields when set", () => {
		const plain = buildRequestBody({ prompt: "x" }, "gpt-5.5", "png", "s");
		expect(plain.tools).toEqual([{ type: "image_generation", output_format: "png" }]);
		const full = buildRequestBody({ prompt: "x", size: "1536x1024", quality: "high", background: "transparent", inputImages: [{ data: "AAAA", mimeType: "image/png" }] }, "gpt-5.5", "webp", "s");
		expect(full.tools[0]).toEqual({ type: "image_generation", output_format: "webp", size: "1536x1024", quality: "high", background: "transparent" });
		expect(full.input[0].content[1]).toEqual({ type: "input_image", image_url: "data:image/png;base64,AAAA" });
	});
});

describe("cli", () => {
	test("parses options and infers format from output extension", () => {
		const options = parseCli(["-o", "out.jpg", "-i", "a.png", "-i", "b.png", "-n", "2", "--size", "1024x1536", "a", "cat"]);
		expect(options).toMatchObject({ prompt: "a cat", output: "out.jpg", format: "jpeg", images: ["a.png", "b.png"], count: 2, size: "1024x1536" });
		expect(parseCli(["-o", "out.bin", "x"])).toMatchObject({ format: undefined });
		expect(() => parseCli(["-f", "gif", "x"])).toThrow("--format");
		expect(() => parseCli(["-n", "0", "x"])).toThrow("--count");
		expect(() => parseCli([])).toThrow("Missing prompt");
		expect(() => parseCli(["-m", "gpt-5.5", "x"])).toThrow("--via-responses");
		expect(parseCli(["--via-responses", "-m", "gpt-6-sol", "x"])).toMatchObject({ viaResponses: true, model: "gpt-6-sol" });
	});

	test("formatProblem refuses formats the route can't deliver, before any request", () => {
		expect(formatProblem("png", false, () => false)).toBeUndefined();
		expect(formatProblem("webp", false, () => true)).toContain("--via-responses");
		expect(formatProblem("jpeg", false, () => false)).toContain("sips");
		expect(formatProblem("jpeg", false, () => true)).toBeUndefined();
		expect(formatProblem("webp", true, () => false)).toBeUndefined();
	});

	test("outputPath handles files, suffixes and directories", async () => {
		const now = new Date("2026-01-02T03:04:05Z");
		expect(await outputPath("/x/out.png", "png", "ig_1", 0, 1, now)).toBe("/x/out.png");
		expect(await outputPath("/x/out", "webp", "ig_1", 0, 1, now)).toBe("/x/out.webp");
		expect(await outputPath("/x/out.png", "png", "ig_1", 1, 3, now)).toBe("/x/out-2.png");
		expect(await outputPath("/x/dir/", "jpeg", "ig_abc", 0, 1, now)).toBe("/x/dir/codex-img-20260102T030405-ig_abc.jpg");
	});
});

describe("auth and request flow", () => {
	let home: string;
	const realFetch = globalThis.fetch;

	beforeEach(async () => {
		home = await mkdtemp(join(tmpdir(), "codex-img-test-"));
		process.env.CODEX_HOME = home;
	});
	afterEach(async () => {
		globalThis.fetch = realFetch;
		delete process.env.CODEX_HOME;
		await rm(home, { recursive: true, force: true });
	});

	const writeAuth = (accessToken: string) => writeFile(join(home, "auth.json"), JSON.stringify({
		auth_mode: "chatgpt", OPENAI_API_KEY: null, tokens: { id_token: "id", access_token: accessToken, refresh_token: "rt_old", account_id: "acct_123" }, last_refresh: "2026-01-01T00:00:00Z",
	}));

	test("reads account id from token claims", () => {
		expect(extractChatGptAccountId(token(3600, "acct_x"))).toBe("acct_x");
		expect(tokenExpiresSoon(token(60))).toBe(true);
		expect(tokenExpiresSoon(token(3600))).toBe(false);
	});

	test("missing auth file is an auth error", async () => {
		const error = await loadCredentials().catch(e => e);
		expect(error).toBeInstanceOf(CodexImageError);
		expect(error.kind).toBe("auth");
	});

	test("expired token fails with a renew hint and makes no request", async () => {
		await writeAuth(token(10));
		const before = await readFile(join(home, "auth.json"), "utf8");
		const fetchMock = mock(async () => sse(imageEvents));
		globalThis.fetch = fetchMock as unknown as typeof fetch;

		const error = await generateImage({ prompt: "a fox" }).catch(e => e);
		expect(error).toBeInstanceOf(CodexImageError);
		expect(error.kind).toBe("auth");
		expect(error.message).toContain("codex login");
		expect(fetchMock).not.toHaveBeenCalled();
		expect(await readFile(join(home, "auth.json"), "utf8")).toBe(before);
	});

	function capture(respond: () => Response) {
		const calls: { url: string; headers: Record<string, string>; body: any }[] = [];
		globalThis.fetch = mock(async (url: string | URL | Request, init?: RequestInit) => {
			calls.push({ url: String(url), headers: init?.headers as Record<string, string>, body: JSON.parse(String(init?.body)) });
			return respond();
		}) as unknown as typeof fetch;
		return calls;
	}

	const directResponse = () => Response.json({
		created: 1, background: "transparent", output_format: "png", quality: "medium", size: "1536x1024",
		data: [{ b64_json: PNG_B64, generation_id: "gen-1" }],
		usage: { input_tokens: 16, output_tokens: 1372, total_tokens: 1388, output_tokens_details: { image_tokens: 1372 } },
	});

	test("direct route posts to images/generations with the prompt verbatim", async () => {
		const valid = token(3600);
		await writeAuth(valid);
		const calls = capture(directResponse);

		const image = await generateImage({ prompt: "a fox", size: "1536x1024", background: "transparent" });
		expect(calls).toHaveLength(1);
		expect(calls[0].url).toBe("https://chatgpt.com/backend-api/codex/images/generations");
		expect(calls[0].headers.Authorization).toBe(`Bearer ${valid}`);
		expect(calls[0].headers["chatgpt-account-id"]).toBe("acct_123");
		expect(calls[0].body).toEqual({ prompt: "a fox", model: "gpt-image-2", size: "1536x1024", background: "transparent" });
		expect(image).toMatchObject({
			transport: "direct", id: "gen-1",
			reported: { size: "1536x1024", quality: "medium", background: "transparent", outputFormat: "png" },
			usage: { input_tokens: 16, output_tokens: 1372, total_tokens: 1388, output_tokens_details: { image_tokens: 1372 } },
		});
		expect(image.revisedPrompt).toBeUndefined();
		expect(image.bytes.subarray(1, 4).toString()).toBe("PNG");
	});

	test("direct route sends reference images to images/edits", async () => {
		await writeAuth(token(3600));
		const calls = capture(directResponse);

		await generateImage({ prompt: "add a hat", inputImages: [{ data: PNG_B64, mimeType: "image/png" }] });
		expect(calls[0].url).toBe("https://chatgpt.com/backend-api/codex/images/edits");
		expect(calls[0].body.images).toEqual([{ image_url: `data:image/png;base64,${PNG_B64}` }]);
	});

	test("direct route rejects a response without image data", async () => {
		await writeAuth(token(3600));
		capture(() => Response.json({ created: 1, data: [] }));
		await expect(generateImage({ prompt: "a fox" })).rejects.toThrow("did not return an image");
	});

	test("responses route streams through a routing model", async () => {
		await writeAuth(token(3600));
		const calls = capture(() => sse(imageEvents));

		const image = await generateImage({ prompt: "a fox", transport: "responses" });
		expect(calls[0].url).toBe("https://chatgpt.com/backend-api/codex/responses");
		expect(calls[0].body.model).toBe("gpt-5.5");
		expect(image).toMatchObject({ transport: "responses", id: "ig_1", routingModel: "gpt-5.5", revisedPrompt: "a fox", responseId: "resp_1" });
		expect(image.reported.model).toBe("gpt-image-2-codex");
	});

	test("401 is an auth error with a renew hint and no retry", async () => {
		await writeAuth(token(3600));
		const fetchMock = mock(async () => new Response("{}", { status: 401 }));
		globalThis.fetch = fetchMock as unknown as typeof fetch;

		const error = await generateImage({ prompt: "a fox" }).catch(e => e);
		expect(error).toBeInstanceOf(CodexImageError);
		expect(error.kind).toBe("auth");
		expect(error.message).toContain("Open Codex");
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	test("rejects gpt-image-* as routing model", async () => {
		await writeAuth(token(3600));
		await expect(generateImage({ prompt: "x", transport: "responses", model: "gpt-image-1" })).rejects.toThrow("routing model");
	});
});
