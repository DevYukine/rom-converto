import { describe, expect, it } from "vitest";
import { buildCliCommand } from "../../composables/useCliEcho";

describe("cmd_nx_merge CLI echo", () => {
  it("builds the nx merge command from staged inputs", () => {
    expect(
      buildCliCommand("cmd_nx_merge", {
        inputs: ["a.nsp", "b.nsp"],
        output: "merged.nsp",
        format: "nsp",
        keys: "",
        onConflict: "overwrite",
        skipSpaceCheck: false,
        taskId: "job-1",
      }),
    ).toBe("> rom-converto nx merge -o merged.nsp a.nsp b.nsp");
  });

  it("includes --format xci and --keys when set", () => {
    expect(
      buildCliCommand("cmd_nx_merge", {
        inputs: ["a.xci"],
        output: "merged.xci",
        format: "xci",
        keys: "C:\\Program Files\\keys\\prod.keys",
        onConflict: "rename",
        skipSpaceCheck: true,
        taskId: "job-2",
      }),
    ).toBe(
      '> rom-converto --skip-space-check nx merge --keys "C:\\Program Files\\keys\\prod.keys" --format xci --on-conflict rename -o merged.xci a.xci',
    );
  });
});

describe("cmd_nx_split CLI echo", () => {
  it("passes outputDir as --output-dir and input positionally", () => {
    expect(
      buildCliCommand("cmd_nx_split", {
        input: "merged.nsp",
        outputDir: "out",
        keys: "",
        onConflict: "overwrite",
        skipSpaceCheck: false,
        taskId: "job-3",
      }),
    ).toBe("> rom-converto nx split --output-dir out merged.nsp");
  });
});
