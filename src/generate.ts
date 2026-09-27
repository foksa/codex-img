// Two routes to the same Codex image backend:
// - "direct" (default): POST /images/generations or /images/edits, as the Codex CLI does.
//   The prompt goes to the image model verbatim and size/background are honoured.
// - "responses": a Responses call where a routing model invokes the image_generation tool.
//   Ported from pi-codex-image-gen (Apache-2.0); kept as a fallback.
import { constants } from "node:fs";
import { open, writeFile } from "node:fs/promises";
import { loadCredentials, type Credentials } from "./auth.js";
import {
	abortable,
	CodexImageError,
	httpFailure,
	MAX_IMAGE_BYTES,
	parseCodexSse,
	readJsonBody,
	reportedImage,
	sanitizeUsage,
	withRequestDeadline,
	type ReportedImage,
} from "./codex-response.js";

export const DEFAULT_MODEL = "gpt-5.5";
// What the Codex CLI requests; the backend decides which model actually serves it.
export const IMAGE_MODEL = "gpt-image-2";
const CODEX_BASE_URL = "https://chatgpt.com/backend-api/codex";
const MAX_RETRIES = 3;
const BASE_DELAY_MS = 1000;
const MAX_RETRY_DELAY_MS = 30_000;
export const MAX_EDIT_IMAGES = 5;
const MAX_INPUT_IMAGE_BYTES = 20 * 1024 * 1024;
const MAX_TOTAL_INPUT_BYTES = 50 * 1024 * 1024;
export const MAX_PROMPT_CHARS = 32_000;

export const OUTPUT_FORMATS = ["png", "jpeg", "webp"] as const;
export type OutputFormat = (typeof OUTPUT_FORMATS)[number];

export interface InputImage {
	data: string;
	mimeType: string;
}

export type Transport = "direct" | "responses";

export interface GenerateOptions {
	prompt: string;
	transport?: Transport;
	/** Routing model, responses transport only. */
	model?: string;
	outputFormat?: OutputFormat;
	size?: string;
	quality?: string;
	background?: string;
	inputImages?: InputImage[];
	sessionId?: string;
	signal?: AbortSignal;
	onProgress?: (stage: string) => void;
}

export interface GeneratedImage {
	bytes: Buffer;
	/** Format of `bytes` as returned by the backend; may differ from the requested one. */
	outputFormat: OutputFormat;
	transport: Transport;
	id: string;
	/** Backend-reported metadata, not independently verified. */
	reported: ReportedImage;
	routingModel?: string;
	revisedPrompt?: string;
	responseId?: string;
	usage?: unknown;
	durationMs: number;
}

// --- Retry helpers ---

export function parseRetryAfter(value: string | null, nowMs = Date.now()): number | undefined {
	if (!value) return undefined;
	const trimmed = value.trim();
	if (/^\d+(?:\.\d+)?$/.test(trimmed)) {
		const milliseconds = Number(trimmed) * 1000;
		return Number.isFinite(milliseconds) ? Math.min(milliseconds, MAX_RETRY_DELAY_MS) : undefined;
	}
	const dateMs = Date.parse(trimmed);
	if (!Number.isFinite(dateMs) || dateMs <= nowMs) return undefined;
	return Math.min(dateMs - nowMs, MAX_RETRY_DELAY_MS);
}

export function retryDelayMs(attempt: number, retryAfter: string | null, random = Math.random, nowMs = Date.now()): number {
	const serverDelay = parseRetryAfter(retryAfter, nowMs);
	if (serverDelay !== undefined) return Math.floor(Math.min(serverDelay * (1 + random() * 0.1), MAX_RETRY_DELAY_MS));
	const exponential = Math.min(BASE_DELAY_MS * 2 ** (attempt - 1), MAX_RETRY_DELAY_MS);
	return Math.floor(exponential * (0.9 + random() * 0.2));
}

function abortableDelay(milliseconds: number, signal: AbortSignal): Promise<void> {
	return abortable(new Promise<void>(resolve => setTimeout(resolve, milliseconds)), signal);
}

// --- Image helpers ---

export function extensionForFormat(outputFormat: OutputFormat): string {
	return outputFormat === "jpeg" ? "jpg" : outputFormat;
}

export function mimeForFormat(outputFormat: OutputFormat): string {
	return `image/${outputFormat}`;
}

function sniffFormat(bytes: Buffer): OutputFormat | undefined {
	if (bytes.length >= 8 && bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) return "png";
	if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return "jpeg";
	if (bytes.length >= 12 && bytes.toString("ascii", 0, 4) === "RIFF" && bytes.toString("ascii", 8, 12) === "WEBP") return "webp";
	return undefined;
}

/** Decode and validate backend image data, returning the format the bytes actually have. */
export function decodeImageData(base64Data: string): { bytes: Buffer; format: OutputFormat } {
	if (base64Data.length > Math.ceil(MAX_IMAGE_BYTES / 3) * 4) throw new Error("Codex image exceeded the 32 MiB size limit.");
	const value = base64Data.trim();
	if (!value || value.length % 4 !== 0 || /[^A-Za-z0-9+/=]/.test(value)) throw new Error("Codex returned invalid base64 image data.");
	const bytes = Buffer.from(value, "base64");
	if (bytes.length === 0 || bytes.length > MAX_IMAGE_BYTES || bytes.toString("base64") !== value) {
		throw new Error("Codex returned invalid base64 image data.");
	}
	const format = sniffFormat(bytes);
	if (!format) throw new Error("Codex returned image data that is not PNG, JPEG or WebP.");
	return { bytes, format };
}

async function readInputImage(path: string): Promise<Buffer> {
	// O_NONBLOCK prevents named pipes from blocking before the regular-file check.
	const file = await open(path, constants.O_RDONLY | (constants.O_NONBLOCK ?? 0));
	try {
		const info = await file.stat();
		if (!info.isFile()) throw new Error("Referenced images must be regular files.");
		if (info.size > MAX_INPUT_IMAGE_BYTES) throw new Error("Referenced image exceeds 20 MiB.");
		const bytes = await file.readFile();
		if (bytes.length > MAX_INPUT_IMAGE_BYTES) throw new Error("Referenced image exceeds 20 MiB.");
		return bytes;
	} finally {
		await file.close();
	}
}

export async function loadInputImages(paths: string[]): Promise<InputImage[]> {
	if (paths.length > MAX_EDIT_IMAGES) throw new Error(`At most ${MAX_EDIT_IMAGES} reference images are supported.`);
	const images: InputImage[] = [];
	let total = 0;
	for (const path of paths) {
		let bytes: Buffer;
		try {
			bytes = await readInputImage(path);
		} catch (error) {
			throw new Error(`Unable to read reference image ${path}: ${error instanceof Error ? error.message : String(error)}`);
		}
		const format = sniffFormat(bytes);
		if (!format) throw new Error(`Reference image is not PNG, JPEG or WebP: ${path}`);
		total += bytes.length;
		if (total > MAX_TOTAL_INPUT_BYTES) throw new Error("Reference images exceed 50 MiB in total.");
		images.push({ data: bytes.toString("base64"), mimeType: mimeForFormat(format) });
	}
	return images;
}

// --- Request ---

export function buildRequestBody(options: GenerateOptions, model: string, outputFormat: OutputFormat, sessionId: string) {
	const tool: Record<string, string> = { type: "image_generation", output_format: outputFormat };
	if (options.size) tool.size = options.size;
	if (options.quality) tool.quality = options.quality;
	if (options.background) tool.background = options.background;
	return {
		model,
		store: false,
		stream: true,
		prompt_cache_key: sessionId,
		instructions:
			"You are generating bitmap image assets. For this request, call the image_generation tool exactly once. Do not answer with only text unless image generation is unavailable.",
		input: [
			{
				role: "user",
				content: [
					{ type: "input_text", text: options.prompt },
					...(options.inputImages ?? []).map(image => ({
						type: "input_image",
						image_url: `data:${image.mimeType};base64,${image.data}`,
					})),
				],
			},
		],
		tools: [tool],
		tool_choice: "auto",
		parallel_tool_calls: false,
		text: { verbosity: "low" },
	};
}

type RouteResult = Omit<GeneratedImage, "bytes" | "outputFormat" | "transport" | "durationMs"> & { result: string };

export function buildDirectBody(options: GenerateOptions) {
	// The images endpoint ignores output_format and always returns PNG; the CLI converts afterwards.
	const body: Record<string, unknown> = { prompt: options.prompt, model: IMAGE_MODEL };
	if (options.size) body.size = options.size;
	if (options.quality) body.quality = options.quality;
	if (options.background) body.background = options.background;
	if (options.inputImages?.length) {
		body.images = options.inputImages.map(image => ({ image_url: `data:${image.mimeType};base64,${image.data}` }));
	}
	return body;
}

/** POST to the Codex backend with bounded retries on 429/5xx. */
async function post(path: string, body: string, accept: string, credentials: Credentials, signal: AbortSignal): Promise<Response> {
	for (let attempt = 1; attempt <= MAX_RETRIES + 1; attempt++) {
		signal.throwIfAborted();
		let response: Response;
		try {
			response = await abortable(fetch(`${CODEX_BASE_URL}/${path}`, {
				method: "POST",
				headers: {
					Authorization: `Bearer ${credentials.accessToken}`,
					"chatgpt-account-id": credentials.accountId,
					originator: "codex_cli_rs",
					"User-Agent": "codex-img",
					"OpenAI-Beta": "responses=experimental",
					accept,
					"content-type": "application/json",
				},
				body,
				signal,
				redirect: "error",
			}), signal);
		} catch {
			signal.throwIfAborted();
			throw new Error("Codex connection failed. No automatic retry was made; check connectivity before trying again.");
		}

		if (!response.ok) {
			const failure = await httpFailure(response, signal);
			if (attempt <= MAX_RETRIES && failure.retry) {
				await abortableDelay(retryDelayMs(attempt, response.headers.get("retry-after")), signal);
				continue;
			}
			throw new CodexImageError(failure.message, failure.kind);
		}
		const debugPath = process.env.CODEX_IMG_DEBUG_RAW;
		if (debugPath) void dumpRaw(response.clone(), debugPath);
		return response;
	}
	throw new Error("Codex image generation request failed after all retries.");
}

async function requestDirect(options: GenerateOptions, credentials: Credentials, signal: AbortSignal): Promise<RouteResult> {
	const edit = Boolean(options.inputImages?.length);
	options.onProgress?.(edit ? "editing" : "generating");
	const response = await post(edit ? "images/edits" : "images/generations", JSON.stringify(buildDirectBody(options)), "application/json", credentials, signal);
	const json = await readJsonBody(response, signal);
	const first = Array.isArray(json.data) ? json.data[0] as Record<string, unknown> | undefined : undefined;
	if (typeof first?.b64_json !== "string" || !first.b64_json) throw new Error("Codex did not return an image.");
	const secrets = [credentials.accessToken, credentials.accountId];
	const id = typeof first.generation_id === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(first.generation_id) ? first.generation_id : crypto.randomUUID();
	options.onProgress?.("completed");
	return {
		result: first.b64_json,
		id,
		reported: reportedImage(json, secrets),
		usage: sanitizeUsage(json.usage),
	};
}

async function requestResponses(options: GenerateOptions, outputFormat: OutputFormat, credentials: Credentials, signal: AbortSignal): Promise<RouteResult> {
	const model = options.model || DEFAULT_MODEL;
	if (model.startsWith("gpt-image-")) {
		throw new Error("--model selects the Codex routing model (e.g. gpt-5.5), not the image model; the backend picks the image model.");
	}
	const body = JSON.stringify(buildRequestBody(options, model, outputFormat, options.sessionId ?? crypto.randomUUID()));
	const response = await post("responses", body, "text/event-stream", credentials, signal);
	const parsed = await parseCodexSse(response, signal, [credentials.accessToken, credentials.accountId], options.onProgress);
	if (!parsed.image) {
		const text = parsed.text.join("").trim();
		throw new Error(text ? `Codex did not return an image. Response text: ${text}` : "Codex did not return an image.");
	}
	return {
		result: parsed.image.result,
		id: parsed.image.id,
		reported: parsed.image.reported,
		usage: parsed.usage,
		routingModel: model,
		revisedPrompt: parsed.image.revisedPrompt,
		responseId: parsed.responseId,
	};
}

/** Debug aid (CODEX_IMG_DEBUG_RAW=<path>): save response headers and the raw SSE stream. */
async function dumpRaw(response: Response, path: string): Promise<void> {
	const headers = [...response.headers].map(([key, value]) => `${key}: ${value}`).join("\n");
	const body = await response.text().catch(error => `<stream error: ${error}>`);
	await writeFile(path, `HTTP ${response.status}\n${headers}\n\n${body}`, { mode: 0o600 });
}

export async function generateImage(options: GenerateOptions): Promise<GeneratedImage> {
	if (!options.prompt.trim() || options.prompt.length > MAX_PROMPT_CHARS) throw new Error("Image prompt must contain 1 to 32,000 characters.");
	const transport = options.transport ?? "direct";
	const outputFormat = options.outputFormat ?? "png";
	const credentials = await loadCredentials();

	const started = Date.now();
	const result = await withRequestDeadline(options.signal, signal => transport === "direct"
		? requestDirect(options, credentials, signal)
		: requestResponses(options, outputFormat, credentials, signal));
	// Never throw away a generated image over a format mismatch: quota is already spent.
	const { bytes, format } = decodeImageData(result.result);
	const { result: _, ...rest } = result;
	return { ...rest, bytes, outputFormat: format, transport, durationMs: Date.now() - started };
}
