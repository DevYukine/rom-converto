import type { Ref } from "vue";
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { BatchItem } from "~/types/batch";

// Read a command's returned plan line. Report-capable commands return a
// { message } object; the rest return a plain string.
function planText(res: unknown): string {
  return typeof res === "object" && res !== null
    ? String((res as { message?: unknown }).message ?? res)
    : String(res);
}

// Drives the dry-run Preview action for a write-capable page. Single mode
// previews one file; batch mode walks the pending queue and accumulates one
// plan line per file. Queue item status is never touched so a later real run
// still processes every file. Nothing is written: the command runs the full
// resolution under dry_run and returns the plan instead of converting.
export function usePreview(commandName: string) {
  const preview = ref("");
  const previewing = ref(false);
  const error = ref("");

  async function single(args: Record<string, unknown>): Promise<void> {
    preview.value = "";
    error.value = "";
    previewing.value = true;
    try {
      const res = await invoke<unknown>(commandName, { ...args, dryRun: true });
      preview.value = planText(res);
    } catch (e) {
      error.value = String(e);
    } finally {
      previewing.value = false;
    }
  }

  async function batch(
    queue: Ref<BatchItem[]>,
    buildArgs: (item: BatchItem) => Record<string, unknown>,
  ): Promise<void> {
    preview.value = "";
    error.value = "";
    previewing.value = true;
    const lines: string[] = [];
    try {
      for (const item of queue.value) {
        if (item.status === "done" || item.status === "error") continue;
        const res = await invoke<unknown>(commandName, {
          ...buildArgs(item),
          dryRun: true,
        });
        lines.push(planText(res));
      }
      preview.value = lines.join("\n");
    } catch (e) {
      error.value = String(e);
    } finally {
      previewing.value = false;
    }
  }

  return { preview, previewing, error, single, batch };
}
