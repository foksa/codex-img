// Reads the ChatGPT login that the Codex CLI keeps in $CODEX_HOME/auth.json.
// Tokens are never refreshed here: refresh tokens rotate, so refreshing outside
// codex could log codex out. Expired logins are left to codex to renew.
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { CodexImageError, RENEW_HINT } from "./codex-response.js";

const JWT_CLAIM_PATH = "https://api.openai.com/auth";
const EXPIRY_MARGIN_MS = 60_000;

export interface Credentials {
	accessToken: string;
	accountId: string;
}

interface AuthFile {
	auth_mode?: string | null;
	OPENAI_API_KEY?: string | null;
	tokens?: {
		id_token?: string;
		access_token?: string;
		refresh_token?: string;
		account_id?: string;
	} | null;
	last_refresh?: string;
	[key: string]: unknown;
}

export function codexHome(): string {
	return process.env.CODEX_HOME || join(homedir(), ".codex");
}

export function authPath(): string {
	return join(codexHome(), "auth.json");
}

export function decodeJwtPayload(token: string): Record<string, unknown> {
	const parts = token.split(".");
	if (parts.length !== 3 || !parts[1]) throw new CodexImageError("Codex access token is not a JWT. Run `codex login` again.", "auth");
	try {
		return JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8")) as Record<string, unknown>;
	} catch {
		throw new CodexImageError("Failed to decode Codex access token. Run `codex login` again.", "auth");
	}
}

export function extractChatGptAccountId(token: string): string {
	const claims = decodeJwtPayload(token)[JWT_CLAIM_PATH];
	const accountId = claims && typeof claims === "object" ? (claims as Record<string, unknown>).chatgpt_account_id : undefined;
	if (typeof accountId !== "string" || !accountId) {
		throw new CodexImageError("Codex access token does not contain chatgpt_account_id. Run `codex login` again.", "auth");
	}
	return accountId;
}

export function tokenExpiresSoon(token: string, nowMs = Date.now()): boolean {
	const exp = decodeJwtPayload(token).exp;
	return typeof exp !== "number" || exp * 1000 - nowMs < EXPIRY_MARGIN_MS;
}

async function readAuthFile(path: string): Promise<AuthFile> {
	let raw: string;
	try {
		raw = await readFile(path, "utf8");
	} catch {
		throw new CodexImageError(`No Codex login found at ${path}. Run \`codex login\` and sign in with ChatGPT.`, "auth");
	}
	try {
		return JSON.parse(raw) as AuthFile;
	} catch {
		throw new CodexImageError(`Codex auth file ${path} is not valid JSON. Run \`codex login\` again.`, "auth");
	}
}

function credentialsFrom(auth: AuthFile): Credentials {
	const accessToken = auth.tokens?.access_token;
	if (!accessToken) {
		const hint = auth.OPENAI_API_KEY ? " Codex is logged in with an API key; image generation here needs a ChatGPT login." : "";
		throw new CodexImageError(`Codex login has no ChatGPT access token.${hint} Run \`codex login\` and sign in with ChatGPT.`, "auth");
	}
	return { accessToken, accountId: auth.tokens?.account_id || extractChatGptAccountId(accessToken) };
}

export interface LoginStatus {
	authPath: string;
	accountId: string;
	expiresAt?: string;
}

/** Offline login check: validates auth.json and the token's expiry without calling the backend. */
export async function loginStatus(): Promise<LoginStatus> {
	const { accessToken, accountId } = await loadCredentials();
	const exp = decodeJwtPayload(accessToken).exp;
	return { authPath: authPath(), accountId, expiresAt: typeof exp === "number" ? new Date(exp * 1000).toISOString() : undefined };
}

export async function loadCredentials(): Promise<Credentials> {
	const credentials = credentialsFrom(await readAuthFile(authPath()));
	if (tokenExpiresSoon(credentials.accessToken)) {
		throw new CodexImageError(`Your Codex login has expired. ${RENEW_HINT}`, "auth");
	}
	return credentials;
}
