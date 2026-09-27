#!/usr/bin/env bun
import { spawnSync } from "node:child_process";
import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import { basename, dirname, extname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { loadCredentials, loginStatus } from "./auth.js";
import { CodexImageError, type ErrorKind } from "./codex-response.js";
import {
	DEFAULT_MODEL,
	extensionForFormat,
	generateImage,
	loadInputImages,
	MAX_EDIT_IMAGES,
	OUTPUT_FORMATS,
	type GeneratedImage,
	type OutputFormat,
} from "./generate.js";

const VERSION = "0.1.0";

const HELP = `codex-img ${VERSION} - generate images with your ChatGPT/Codex subscription

Usage:
  codex-img [options] "<prompt>"
  echo "<prompt>" | codex-img [options] -
  codex-img status [--json]   Check the Codex login offline (uses no quota)

Options:
  -o, --output <path>       Output file or directory (default: current directory)
  -i, --image <path>        Reference image to edit/compose (repeatable, max ${MAX_EDIT_IMAGES})
  -f, --format <fmt>        png | jpeg | webp (default: from -o extension, else png).
                            The direct route returns PNG; jpeg is converted locally
                            with macOS sips, webp needs --via-responses
  -s, --size <WxH>          e.g. 1024x1024, 1536x1024, 1024x1536, auto
  -q, --quality <q>         low | medium | high | auto
  -b, --background <bg>     transparent | opaque | auto
      --via-responses       Fallback route: a routing model calls the image tool
                            (prompt may be rewritten, --size is ignored)
  -m, --model <model>       Routing model for --via-responses (default: ${DEFAULT_MODEL})
  -n, --count <n>           Number of images, generated in parallel (default: 1)
      --json                Print one JSON object per image to stdout
      --quiet               No progress on stderr
  -h, --help                Show help
  -v, --version             Show version

Uses the ChatGPT login stored by \`codex login\` ($CODEX_HOME/auth.json).
Exit codes: 0 ok, 1 error, 2 auth, 3 quota, 4 moderation, 64 usage.

Example:
  codex-img "flat vector red fox in snow" -o fox.png --json`;

const EXIT_CODES: Record<ErrorKind, number> = { other: 1, auth: 2, quota: 3, moderation: 4 };

export interface CliOptions {
	prompt: string;
	output?: string;
	images: string[];
	format?: OutputFormat;
	size?: string;
	quality?: string;
	background?: string;
	model?: string;
	viaResponses: boolean;
	count: number;
	json: boolean;
	quiet: boolean;
}

class UsageError extends Error {}

function oneOf<T extends string>(name: string, value: string | undefined, allowed: readonly T[]): T | undefined {
	if (value === undefined) return undefined;
	if (!allowed.includes(value as T)) throw new UsageError(`--${name} must be one of: ${allowed.join(", ")}`);
	return value as T;
}

export function parseCli(argv: string[]): CliOptions | "help" | "version" {
	const { values, positionals } = parseArgs({
		args: argv,
		allowPositionals: true,
		options: {
			output: { type: "string", short: "o" },
			image: { type: "string", short: "i", multiple: true },
			format: { type: "string", short: "f" },
			size: { type: "string", short: "s" },
			quality: { type: "string", short: "q" },
			background: { type: "string", short: "b" },
			model: { type: "string", short: "m" },
			"via-responses": { type: "boolean" },
			count: { type: "string", short: "n" },
			json: { type: "boolean" },
			quiet: { type: "boolean" },
			help: { type: "boolean", short: "h" },
			version: { type: "boolean", short: "v" },
		},
	});
	if (values.help) return "help";
	if (values.version) return "version";
	if (positionals.length === 0) throw new UsageError("Missing prompt.");

	let format = oneOf("format", values.format === "jpg" ? "jpeg" : values.format, OUTPUT_FORMATS);
	if (!format && values.output) {
		const ext = extname(values.output).slice(1).toLowerCase();
		format = OUTPUT_FORMATS.find(candidate => candidate === (ext === "jpg" ? "jpeg" : ext));
	}
	if (values.size && !/^(auto|[1-9]\d{1,4}x[1-9]\d{1,4})$/.test(values.size)) throw new UsageError("--size must be WIDTHxHEIGHT or auto.");
	const count = values.count === undefined ? 1 : Number(values.count);
	if (!Number.isInteger(count) || count < 1 || count > 10) throw new UsageError("--count must be an integer from 1 to 10.");
	if (values.model && !values["via-responses"]) throw new UsageError("--model only applies with --via-responses; the direct route has no routing model.");
	const images = values.image ?? [];
	if (images.length > MAX_EDIT_IMAGES) throw new UsageError(`At most ${MAX_EDIT_IMAGES} --image references are supported.`);

	return {
		prompt: positionals.join(" "),
		output: values.output,
		images,
		format,
		size: values.size,
		quality: oneOf("quality", values.quality, ["low", "medium", "high", "auto"] as const),
		background: oneOf("background", values.background, ["transparent", "opaque", "auto"] as const),
		model: values.model,
		viaResponses: values["via-responses"] ?? false,
		count,
		json: values.json ?? false,
		quiet: values.quiet ?? false,
	};
}

async function isDirectory(path: string): Promise<boolean> {
	return stat(path).then(info => info.isDirectory(), () => false);
}

/** Resolve where image `index` of `count` goes. Directories get generated names; files get -N suffixes when count > 1. */
export async function outputPath(output: string | undefined, format: OutputFormat, id: string, index: number, count: number, now = new Date()): Promise<string> {
	const ext = extensionForFormat(format);
	const stamp = now.toISOString().replace(/[-:]/g, "").replace(/\..+/, "");
	const generated = `codex-img-${stamp}-${id.replace(/[^a-zA-Z0-9_-]/g, "_").slice(-12)}.${ext}`;
	if (!output) return resolve(generated);
	if (output.endsWith("/") || await isDirectory(output)) return resolve(output, generated);
	const current = extname(output);
	if (count === 1) return resolve(current ? output : `${output}.${ext}`);
	const stem = current ? basename(output, current) : basename(output);
	return resolve(dirname(output), `${stem}-${index + 1}${current || `.${ext}`}`);
}

function hasSips(): boolean {
	return process.platform === "darwin" && spawnSync("sips", ["--help"], { stdio: "ignore" }).status === 0;
}

/** Refuse, before spending quota, formats the chosen route cannot deliver. */
export function formatProblem(format: OutputFormat, viaResponses: boolean, sipsAvailable: () => boolean): string | undefined {
	if (viaResponses || format === "png") return undefined;
	if (format === "webp") return "WebP output needs --via-responses: the direct endpoint only returns PNG and it can't be converted to WebP locally.";
	if (!sipsAvailable()) return "JPEG output on the direct route needs macOS `sips` to convert the PNG the endpoint returns. Use PNG or --via-responses.";
	return undefined;
}

/**
 * Write the image as `format`, converting PNG to JPEG with sips when needed. If the bytes can't be
 * turned into the requested format, keep them under their real extension: the quota is already spent.
 */
async function saveImage(bytes: Buffer, actual: OutputFormat, format: OutputFormat, path: string, warn: (message: string) => void): Promise<string> {
	await mkdir(dirname(path), { recursive: true });
	if (actual === format) {
		await writeFile(path, bytes, { flag: "wx" });
		return path;
	}
	if (actual === "png" && format === "jpeg" && hasSips()) {
		if (await stat(path).then(() => true, () => false)) throw new Error(`Refusing to overwrite ${path}.`);
		const temp = `${path}.codex-img-${process.pid}.png`;
		await writeFile(temp, bytes, { flag: "wx" });
		try {
			const result = spawnSync("sips", ["-s", "format", "jpeg", "-s", "formatOptions", "high", temp, "--out", path], { stdio: "ignore" });
			if (result.status === 0) return path;
		} finally {
			await rm(temp, { force: true });
		}
	}
	const fallback = path.slice(0, path.length - extname(path).length) + `.${extensionForFormat(actual)}`;
	await writeFile(fallback, bytes, { flag: "wx" });
	warn(`backend returned ${actual}, not ${format}; saved as ${fallback}`);
	return fallback;
}

async function readStdin(): Promise<string> {
	const chunks: Buffer[] = [];
	for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
	return Buffer.concat(chunks).toString("utf8").trim();
}

function describe(path: string, image: GeneratedImage) {
	const { reported } = image;
	return {
		path,
		format: image.outputFormat,
		bytes: image.bytes.length,
		transport: image.transport,
		imageModel: reported.model,
		routingModel: image.routingModel,
		size: reported.size,
		quality: reported.quality,
		background: reported.background,
		revisedPrompt: image.revisedPrompt,
		generationId: image.id,
		responseId: image.responseId,
		usage: image.usage,
		durationMs: image.durationMs,
	};
}

async function status(json: boolean): Promise<number> {
	const info = await loginStatus();
	if (json) console.log(JSON.stringify({ ok: true, ...info }));
	else console.log(`Logged in (account ${info.accountId}); token valid until ${info.expiresAt ?? "unknown"}.\nAuth file: ${info.authPath}`);
	return 0;
}

async function main(argv: string[]): Promise<number> {
	if (argv[0] === "status" && argv.slice(1).every(arg => arg === "--json")) return status(argv.includes("--json"));
	let options: ReturnType<typeof parseCli>;
	try {
		options = parseCli(argv);
	} catch (error) {
		console.error(`codex-img: ${error instanceof Error ? error.message : String(error)}\nRun \`codex-img --help\` for usage.`);
		return 64;
	}
	if (options === "help") return console.log(HELP), 0;
	if (options === "version") return console.log(VERSION), 0;
	const opts = options;

	if (opts.prompt === "-") opts.prompt = await readStdin();
	const format = opts.format ?? "png";
	const problem = formatProblem(format, opts.viaResponses, hasSips);
	if (problem) {
		console.error(`codex-img: ${problem}`);
		return 64;
	}
	const log = (message: string) => { if (!opts.quiet) console.error(message); };

	const controller = new AbortController();
	process.once("SIGINT", () => controller.abort(new Error("Image generation was aborted.")));

	await loadCredentials(); // fail fast, once, on a missing or expired login
	const inputImages = await loadInputImages(opts.images);
	const sessionId = crypto.randomUUID();
	if (opts.output?.endsWith("/")) await mkdir(opts.output, { recursive: true });

	log(`Requesting ${opts.count > 1 ? `${opts.count} images` : "image"}${inputImages.length ? ` with ${inputImages.length} reference(s)` : ""}...`);
	const results = await Promise.allSettled(Array.from({ length: opts.count }, async (_, index) => {
		const tag = opts.count > 1 ? `[${index + 1}] ` : "";
		const image = await generateImage({
			prompt: opts.prompt,
			transport: opts.viaResponses ? "responses" : "direct",
			model: opts.model,
			outputFormat: format,
			size: opts.size,
			quality: opts.quality,
			background: opts.background,
			inputImages,
			sessionId,
			signal: controller.signal,
			onProgress: stage => log(`${tag}${stage}`),
		});
		const target = await outputPath(opts.output, format, image.id, index, opts.count);
		const path = await saveImage(image.bytes, image.outputFormat, format, target, message => console.error(`codex-img: ${tag}warning: ${message}`));
		const info = { ...describe(path, image), format: extname(path) === ".jpg" ? "jpeg" : extname(path).slice(1), bytes: (await stat(path)).size };
		if (opts.json) console.log(JSON.stringify(info));
		else console.log(path);
		log(`${tag}saved ${path} (${info.size ?? "?"}, ${(info.durationMs / 1000).toFixed(1)}s)`);
	}));

	let exitCode = 0;
	for (const result of results) {
		if (result.status === "fulfilled") continue;
		const error = result.reason;
		console.error(`codex-img: ${error instanceof Error ? error.message : String(error)}`);
		exitCode = Math.max(exitCode, error instanceof CodexImageError ? EXIT_CODES[error.kind] : 1);
	}
	return exitCode;
}

if (import.meta.main) {
	main(process.argv.slice(2)).then(
		code => process.exit(code),
		error => {
			console.error(`codex-img: ${error instanceof Error ? error.message : String(error)}`);
			process.exit(error instanceof CodexImageError ? EXIT_CODES[error.kind] : 1);
		},
	);
}
