import rawManifest from "../types/generated/cli_echo.json";
import type { CliEchoManifest, CliFlag } from "../types/generated/cli_echo";

const manifest = rawManifest as CliEchoManifest;
const BINARY = "rom-converto";

// Values a POSIX shell treats literally: alphanumerics plus a few safe
// punctuation marks. Anything else is quoted, so regexes, globs, and paths
// with spaces survive the trip.
const PLAIN = /^[A-Za-z0-9_\-.,/:@+=]+$/;

// Windows shells treat single quotes as literal characters, so values there
// get the MSVC argv encoding: double quotes, doubled inner quotes, and
// doubled backslash runs that precede a quote or close the value.
let WINDOWS = typeof navigator !== "undefined" && /win/i.test(navigator.platform ?? "");

// Overridable for tests; the GUI never changes it at runtime.
export function setWindowsQuoting(enabled: boolean): void {
	WINDOWS = enabled;
}

function windowsQuote(s: string): string {
	let out = '"';
	let slashes = 0;
	for (const ch of s) {
		if (ch === "\\") {
			slashes++;
		} else if (ch === '"') {
			out += "\\".repeat(slashes * 2 + 1) + '"';
			slashes = 0;
		} else {
			out += "\\".repeat(slashes) + ch;
			slashes = 0;
		}
	}
	return out + "\\".repeat(slashes * 2) + '"';
}

function quote(v: unknown): string {
	const s = v == null ? "" : String(v);
	if (s === "") return '""';
	if (PLAIN.test(s)) return s;
	if (WINDOWS) return windowsQuote(s);
	return `'${s.replaceAll("'", `'\\''`)}'`;
}

function flagToken(def: CliFlag, value: unknown): string | false {
	const { kind, flag, explicit_off } = def;
	if (kind === "bool") {
		if (value === true) return flag;
		// An explicit false overrides a config true; omitting the flag
		// would let the config value apply.
		return value === false && explicit_off && `${flag}=false`;
	}
	if (kind === "list" || kind === "repeated" || kind === "equals_list") {
		if (!Array.isArray(value)) return false;
		if (value.length === 0) {
			// The bare form of an equals-list flag selects the flag's own
			// default (--remove-headers strips every detected header);
			// other empty lists either drop out or, when the flag has an
			// explicit empty form, ride on =.
			if (kind === "equals_list") return flag;
			return explicit_off && `${flag}=`;
		}
		const rendered = value.map((v) => quote(v));
		if (kind === "equals_list") return `${flag}=${quote(value.join(","))}`;
		if (kind === "repeated") {
			return value
				.map((v, i) => {
					const token = rendered[i];
					return typeof v === "string" && v.startsWith("-")
						? `${flag}=${token}`
						: `${flag} ${token}`;
				})
				.join(" ");
		}
		return `${flag} ${rendered.join(",")}`;
	}
	if (value == null) return false;
	// An empty string is the explicit empty value; a value starting with
	// '-' would parse as a flag, so it rides on =.
	const text = Array.isArray(value) ? value.map((v) => String(v)).join(",") : value;
	return text === ""
		? `${flag}=`
		: typeof text === "string" && text.startsWith("-")
			? `${flag}=${quote(text)}`
			: `${flag} ${quote(text)}`;
}

// Builds the copyable `rom-converto ...` command for a `cmd_run` payload
// (`{ request: { operation, input, output, options, dry_run }, reportFile }`
// from lib/opdefs/types.ts `runArgs`), deriving the CLI's shape entirely
// from the generated cli_echo manifest.
export function buildCliCommand(payload: Record<string, unknown>): string {
	const request = payload.request as Record<string, unknown> | undefined;
	if (!request) return "";
	const operation = String(request.operation ?? "");
	const path = manifest.ops[operation];
	const opFlags = manifest.op_flags[operation];
	if (!path || !opFlags) return "";
	const options: Record<string, unknown> = {
		...(request.options as Record<string, unknown>),
		report: payload.reportFile ?? undefined,
	};

	const tokens: string[] = [BINARY];
	if (request.dry_run === true) tokens.push("--dry-run");
	if (options.skip_space_check === true) tokens.push("--skip-space-check");
	if (options.config) tokens.push("--config", quote(options.config));
	if (options.preset) tokens.push("--preset", quote(options.preset));
	tokens.push(...path);

	const inputs = options.inputs;
	if (Array.isArray(inputs) && inputs.length > 0) {
		for (const entry of inputs) {
			const p = typeof entry === "string" ? entry : (entry as { path?: unknown } | null)?.path;
			tokens.push(quote(p));
		}
	} else if (request.input) {
		tokens.push(quote(request.input));
	}

	const output = request.output;
	if (output && !options.output_template) {
		const kind = manifest.output[operation];
		if (kind === "output_dir") tokens.push("--output-dir", quote(output));
		else if (kind === "output_flag") tokens.push("--output", quote(output));
		else if (kind !== "none") tokens.push(quote(output));
	}

	for (const field of opFlags) {
		// Other ops get the same policy from the global default, so echoing
		// it would be noise; organize resolves an unset policy itself and
		// must always show the flag.
		if (field === "on_conflict" && options.on_conflict === "overwrite" && operation !== "organize") continue;
		const def = manifest.flags[field];
		if (!def) continue;
		const token = flagToken(def, options[field]);
		if (token) tokens.push(token);
	}

	return tokens.join(" ");
}
