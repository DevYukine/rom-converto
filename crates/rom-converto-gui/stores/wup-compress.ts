import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

/// True when the input path ends in `.wud` or `.wux`, so the UI
/// knows to prompt for a master key. Mirrors the Rust extension check.
export function isDiscInput(input: string): boolean {
  const lower = input.toLowerCase();
  return lower.endsWith(".wud") || lower.endsWith(".wux");
}

export const useWupCompressStore = defineStore("wup-compress", () => {
  // Title inputs bundled into one .wua. Directories are loadiine or
  // NUS; files with .wud/.wux are disc images needing a master key.
  const queue = ref<BatchItem[]>([]);
  const output = ref("");
  // Zstd level: 0 = Cemu default (6), 1..22 = explicit.
  const level = ref<number>(0);
  // Master key per disc input, keyed by BatchItem.id.
  const keys = ref<Record<string, string>>({});

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function addToQueue(inputPath: string) {
    if (queue.value.some((i) => i.input === inputPath)) return;
    queue.value.push({
      id: crypto.randomUUID(),
      input: inputPath,
      output: "",
      status: "pending",
    });
  }

  function removeFromQueue(id: string) {
    queue.value = queue.value.filter((item) => item.id !== id);
    delete keys.value[id];
  }

  function clearQueue() {
    queue.value = [];
    keys.value = {};
  }

  function setKey(id: string, keyPath: string) {
    if (keyPath) {
      keys.value[id] = keyPath;
    } else {
      delete keys.value[id];
    }
  }

  /// Positional `keys` array aligned with `inputs`: one entry per
  /// disc input (empty if unset), nothing for directories.
  function collectKeys(): string[] {
    const out: string[] = [];
    for (const item of queue.value) {
      if (isDiscInput(item.input)) {
        out.push(keys.value[item.id] ?? "");
      }
    }
    return out;
  }

  function $reset() {
    queue.value = [];
    output.value = "";
    level.value = 0;
    keys.value = {};
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    queue,
    output,
    level,
    keys,
    result,
    error,
    loading,
    addToQueue,
    removeFromQueue,
    clearQueue,
    setKey,
    collectKeys,
    $reset,
  };
});
