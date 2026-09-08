import { describe, expect, it } from "vitest";
import { buildCliCommand } from "./useCliEcho";
import { runArgs } from "../lib/opdefs/types";

describe("buildCliCommand", () => {
  it("orders global flags dry-run, then skip-space-check, ahead of the op path", () => {
    const payload = runArgs("hash", "game.iso", null, { skip_space_check: true, algo: "crc32" }, true, "t1");
    expect(buildCliCommand(payload)).toBe("> rom-converto --dry-run --skip-space-check hash game.iso --algo crc32");
  });

  it("joins a list-kind flag with commas", () => {
    const payload = runArgs("chd.compress", "game.iso", "game.chd", { codecs: ["cdlz", "cdzl"] }, false, "t2");
    expect(buildCliCommand(payload)).toBe("> rom-converto chd compress game.iso game.chd --codecs cdlz,cdzl");
  });

  it("emits options.inputs as positional args instead of the single input", () => {
    const payload = runArgs("nx.merge", "a.nsp", "merged.nsp", { inputs: ["a.nsp", "b.nsp"] }, false, "t3");
    expect(buildCliCommand(payload)).toBe("> rom-converto nx merge a.nsp b.nsp --output merged.nsp");
  });

  it("routes an output_dir-kind op's output through --output-dir", () => {
    const payload = runArgs("nx.split", "merged.nsp", "out", {}, false, "t3b");
    expect(buildCliCommand(payload)).toBe("> rom-converto nx split merged.nsp --output-dir out");
  });

  it("routes an output_flag-kind op's output through --output", () => {
    const payload = runArgs("wup.compress", null, "out.wua", { level: 6, inputs: ["a.wud"] }, false, "t3c");
    expect(buildCliCommand(payload)).toBe("> rom-converto wup compress a.wud --output out.wua --level 6");
  });

  it("drops on_conflict when it's the overwrite default, keeps other values", () => {
    const overwrite = runArgs("chd.compress", "a.iso", "a.chd", { on_conflict: "overwrite" }, false, "t4");
    const rename = runArgs("chd.compress", "a.iso", "a.chd", { on_conflict: "rename" }, false, "t5");
    expect(buildCliCommand(overwrite)).toBe("> rom-converto chd compress a.iso a.chd");
    expect(buildCliCommand(rename)).toBe("> rom-converto chd compress a.iso a.chd --on-conflict rename");
  });

  it("suppresses the positional output when an output template is set", () => {
    const payload = runArgs("chd.compress", "a.iso", "a.chd", { output_template: "{title}.chd" }, false, "t6");
    expect(buildCliCommand(payload)).toBe("> rom-converto chd compress a.iso --output-template {title}.chd");
  });

  it("emits a bool-kind flag only when true", () => {
    const payload = runArgs("dol.verify", "a.dol", null, { full: false }, false, "t7");
    expect(buildCliCommand(payload)).toBe("> rom-converto dol verify a.dol");
  });

  it("quotes flag values containing spaces", () => {
    const payload = runArgs(
      "nx.merge",
      "a.xci",
      "merged.xci",
      { inputs: ["a.xci"], format: "xci", keys: "C:\\Program Files\\keys\\prod.keys", skip_space_check: true },
      false,
      "t8",
    );
    expect(buildCliCommand(payload)).toBe(
      '> rom-converto --skip-space-check nx merge a.xci --output merged.xci --format xci --keys "C:\\Program Files\\keys\\prod.keys"',
    );
  });

  it("emits --report from reportFile, not from the options", () => {
    const payload = runArgs("chd.compress", "game.iso", "game.chd", {}, true, "t9", "report.json");
    expect(buildCliCommand(payload)).toBe(
      "> rom-converto --dry-run chd compress game.iso game.chd --report report.json",
    );
  });

  it("unwraps object-shaped positional inputs", () => {
    const payload = runArgs(
      "wup.compress",
      null,
      "out.wua",
      {
        level: 6,
        inputs: [
          { path: "a.wud", format: "disc", key: null, key_path: null },
          { path: "b.app", format: null, key: "k.txt", key_path: null },
        ],
      },
      false,
      "t10",
    );
    expect(buildCliCommand(payload)).toBe("> rom-converto wup compress a.wud b.app --output out.wua --level 6");
  });

  it("returns an empty string for a payload without a request", () => {
    expect(buildCliCommand({ taskId: "t11" })).toBe("");
  });
});
