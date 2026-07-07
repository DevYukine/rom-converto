const BINARY = "rom-converto";

function quote(p: string): string {
  return p.includes(" ") ? `"${p}"` : p;
}

function str(v: unknown): string {
  return v == null ? "" : String(v);
}

function join(parts: Array<string | false | null | undefined>): string {
  const tokens = parts.filter((p): p is string => typeof p === "string" && p.length > 0);
  return `> ${BINARY} ${tokens.join(" ")}`;
}

function discSub(args: Record<string, unknown>, verb: string): string[] {
  const family = str(args.taskId).startsWith("dol") ? "dol" : "rvl";
  return [family, verb];
}

function conflict(args: Record<string, unknown>): string | false {
  const value = str(args.onConflict);
  return value && value !== "overwrite" ? `--on-conflict ${value}` : false;
}

function template(args: Record<string, unknown>): string | false {
  const value = str(args.outputTemplate);
  return value && `--output-template ${quote(value)}`;
}

function report(args: Record<string, unknown>): string | false {
  const value = str(args.reportFile);
  return value && `--report ${quote(value)}`;
}

// When a template is set it names the exact output path, so the positional
// output token is dropped to mirror the CLI's conflicts_with rule.
function outputArg(args: Record<string, unknown>): string | false {
  if (str(args.outputTemplate)) return false;
  const value = str(args.output);
  return value && quote(value);
}

export function buildCliCommand(command: string, args: Record<string, unknown>): string {
  switch (command) {
    case "cmd_chd_compress": {
      const mode = str(args.mode);
      const hunk = args.hunkSize as number | null | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "chd", "compress",
        mode === "dvd" && "--dvd",
        mode === "cd" && "--cd",
        args.zstd === true && "--zstd",
        hunk ? `--hunk-size ${hunk}` : false,
        conflict(args),
        template(args),
        report(args),
        quote(str(args.inputPath)),
        outputArg(args),
      ]);
    }
    case "cmd_chd_extract": {
      const parent = str(args.parent);
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "chd", "extract",
        parent && `--parent ${quote(parent)}`,
        template(args),
        report(args),
        quote(str(args.input)),
        outputArg(args),
      ]);
    }
    case "cmd_chd_verify": {
      const parent = str(args.parent);
      return join([
        "chd", "verify",
        parent && `--parent ${quote(parent)}`,
        args.fix === true && "--fix",
        quote(str(args.input)),
      ]);
    }
    case "cmd_cso_compress": {
      const block = args.blockSize as number | null | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "cso", "compress",
        args.format === "zso" && "--format zso",
        block ? `--block-size ${block}` : false,
        conflict(args),
        template(args),
        report(args),
        quote(str(args.inputPath)),
        outputArg(args),
      ]);
    }
    case "cmd_cso_decompress":
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "cso", "decompress",
        conflict(args),
        template(args),
        report(args),
        quote(str(args.inputPath)),
        outputArg(args),
      ]);
    case "cmd_cso_verify":
      return join([
        "cso", "verify",
        args.full === true && "--full",
        quote(str(args.inputPath)),
      ]);
    case "cmd_cue_merge":
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "cue", "merge",
        conflict(args),
        quote(str(args.cuePath)),
        quote(str(args.output)),
      ]);
    case "cmd_cdn_to_cia": {
      const output = str(args.output);
      return join([
        args.skipSpaceCheck === true && "--skip-space-check",
        "ctr", "cdn-to-cia",
        args.decrypt === true && "-D",
        args.compress === true && "-Z",
        args.cleanup === true && "-C",
        args.recursive === true && "-R",
        args.ensureTicketExists === true && "-T",
        conflict(args),
        quote(str(args.cdnDir)),
        output && quote(output),
      ]);
    }
    case "cmd_generate_ticket":
      return join([
        "ctr", "generate-cdn-ticket",
        quote(str(args.cdnDir)),
        quote(str(args.output)),
      ]);
    case "cmd_compress_rom": {
      const level = args.level as number | null | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "ctr", "compress",
        level ? `-l ${level}` : false,
        args.allowEncrypted === true && "--allow-encrypted",
        conflict(args),
        template(args),
        quote(str(args.input)),
        outputArg(args),
      ]);
    }
    case "cmd_decompress_rom":
      return join([args.dryRun === true && "--dry-run", args.skipSpaceCheck === true && "--skip-space-check", "ctr", "decompress", conflict(args), template(args), quote(str(args.input)), outputArg(args)]);
    case "cmd_decrypt_rom":
      return join([args.dryRun === true && "--dry-run", args.skipSpaceCheck === true && "--skip-space-check", "ctr", "decrypt", conflict(args), template(args), quote(str(args.input)), outputArg(args)]);
    case "cmd_encrypt_rom":
      return join([args.dryRun === true && "--dry-run", args.skipSpaceCheck === true && "--skip-space-check", "ctr", "encrypt", conflict(args), template(args), quote(str(args.input)), outputArg(args)]);
    case "cmd_convert_ctr":
      return join([args.dryRun === true && "--dry-run", args.skipSpaceCheck === true && "--skip-space-check", "ctr", "convert", conflict(args), template(args), quote(str(args.input)), outputArg(args)]);
    case "cmd_verify_ctr":
      return join([
        "ctr", "verify",
        args.verifyContent === true && "--full",
        quote(str(args.input)),
      ]);
    case "cmd_compress_disc": {
      const level = args.level as number | undefined;
      const chunk = args.chunkSize as number | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        ...discSub(args, "compress"),
        level != null && level !== 22 ? `-l ${level}` : false,
        chunk != null && chunk !== 131072 ? `--chunk-size ${chunk}` : false,
        conflict(args),
        template(args),
        report(args),
        quote(str(args.input)),
        outputArg(args),
      ]);
    }
    case "cmd_decompress_disc":
      return join([args.dryRun === true && "--dry-run", args.skipSpaceCheck === true && "--skip-space-check", ...discSub(args, "decompress"), conflict(args), template(args), report(args), quote(str(args.input)), outputArg(args)]);
    case "cmd_verify_dol":
      return join(["dol", "verify", args.full === true && "--full", quote(str(args.input))]);
    case "cmd_verify_rvl":
      return join(["rvl", "verify", args.full === true && "--full", quote(str(args.input))]);
    case "cmd_nx_compress": {
      const keys = str(args.keys);
      const level = args.level as number | undefined;
      const mode = str(args.mode);
      const exp = args.blockSizeExp as number | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "nx", "compress",
        keys && `--keys ${quote(keys)}`,
        level != null && level !== 18 ? `-l ${level}` : false,
        mode === "block" && "--mode block",
        mode === "block" && exp != null && exp !== 20 ? `--block-size-exp ${exp}` : false,
        conflict(args),
        template(args),
        report(args),
        quote(str(args.input)),
        outputArg(args),
      ]);
    }
    case "cmd_nx_decompress": {
      const keys = str(args.keys);
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "nx", "decompress",
        keys && `--keys ${quote(keys)}`,
        conflict(args),
        template(args),
        report(args),
        quote(str(args.input)),
        outputArg(args),
      ]);
    }
    case "cmd_nx_verify": {
      const keys = str(args.keys);
      return join(["nx", "verify", keys && `--keys ${quote(keys)}`, quote(str(args.input))]);
    }
    case "cmd_wup_compress": {
      const inputs = Array.isArray(args.inputs) ? (args.inputs as string[]) : [];
      const keys = Array.isArray(args.keys) ? (args.keys as string[]) : [];
      const level = args.level as number | undefined;
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "wup", "compress",
        `-o ${quote(str(args.output))}`,
        level ? `-l ${level}` : false,
        conflict(args),
        ...keys.filter((k) => k).map((k) => `--key ${quote(k)}`),
        ...inputs.map((i) => quote(i)),
      ]);
    }
    case "cmd_wup_decrypt":
      return join([
        args.dryRun === true && "--dry-run",
        args.skipSpaceCheck === true && "--skip-space-check",
        "wup", "decrypt",
        `-o ${quote(str(args.output))}`,
        conflict(args),
        quote(str(args.input)),
      ]);
    case "cmd_wup_verify": {
      const keys = str(args.keys);
      return join(["wup", "verify", keys && `--key ${quote(keys)}`, quote(str(args.input))]);
    }
    case "cmd_hash": {
      const algos = Array.isArray(args.algos) ? (args.algos as string[]) : [];
      const depth = args.maxDepth as number | null | undefined;
      return join([
        "hash",
        algos.length > 0 && `--algo ${algos.join(",")}`,
        args.recursive === true && "-R",
        args.recursive === true && depth != null ? `--max-depth ${depth}` : false,
        quote(str(args.input)),
      ]);
    }
    case "cmd_playlist": {
      const outputDir = str(args.outputDir);
      const depth = args.maxDepth as number | null | undefined;
      const policy = str(args.onConflict);
      return join([
        "playlist",
        outputDir && `--output-dir ${quote(outputDir)}`,
        args.mode === "always" && "--playlist-mode always",
        `--ext ${str(args.extensions)}`,
        depth != null ? `--max-depth ${depth}` : false,
        policy && `--on-conflict ${policy}`,
        quote(str(args.scanDir)),
      ]);
    }
    case "cmd_dat_verify":
      return join(["dat", "verify", args.quick === true && "--quick", quote(str(args.input))]);
    case "cmd_dat_scan": {
      const depth = args.maxDepth as number | null | undefined;
      const algos = Array.isArray(args.algos) ? args.algos.join(",") : "";
      return join([
        "dat", "scan",
        algos !== "" && algos !== "crc32" && `--algo ${algos}`,
        args.quick === true && "--quick",
        depth != null ? `--max-depth ${depth}` : false,
        quote(str(args.input)),
      ]);
    }
    case "cmd_dat_rename": {
      const depth = args.maxDepth as number | null | undefined;
      const policy = str(args.onConflict);
      return join([
        "dat", "rename",
        "-R",
        depth != null ? `--max-depth ${depth}` : false,
        args.dryRun === true && "--dry-run",
        policy && `--on-conflict ${policy}`,
        quote(str(args.input)),
      ]);
    }
    default:
      return "";
  }
}
