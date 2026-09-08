import rawManifest from "../types/generated/cli_echo.json";
import type { CliEchoManifest } from "../types/generated/cli_echo";

const manifest = rawManifest as CliEchoManifest;
const BINARY = "rom-converto";

function quote(v: unknown): string {
	const s = v == null ? "" : String(v);
	return s.includes(" ") ? `"${s}"` : s;
}

function flagToken(kind: string, flag: string, value: unknown): string | false {
	if (kind === "bool") return value === true && flag;
	if (kind === "list") return Array.isArray(value) && value.length > 0 && `${flag} ${value.join(",")}`;
	return value != null && value !== "" && `${flag} ${quote(value)}`;
}

// Builds the `> rom-converto ...` preview for a `cmd_run` payload
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
		if (field === "on_conflict" && options.on_conflict === "overwrite") continue;
		const def = manifest.flags[field];
		if (!def) continue;
		const token = flagToken(def.kind, def.flag, options[field]);
		if (token) tokens.push(token);
	}

	return `> ${tokens.join(" ")}`;
}
