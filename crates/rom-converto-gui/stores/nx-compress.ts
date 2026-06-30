import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export type NxMode = "solid" | "block";

export function isXciInput(input: string): boolean {
  return input.toLowerCase().endsWith(".xci");
}

export const useNxCompressStore = defineStore("nx-compress", () => {
  const queue = ref<BatchItem[]>([]);
  const recursive = ref(true);
  const maxDepth = ref<number | null>(null);
  const output = ref("");
  const keys = ref("");
  const level = ref<number>(18);
  // Defaults follow nsz: solid for NSP, block for XCI. Switching to
  // block while a queue contains an XCI is a no-op; the auto switch
  // only kicks in when the user has not deliberately picked.
  const mode = ref<NxMode>("solid");
  const blockSizeExp = ref<number>(20);
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");
  const reportFile = ref("");
  const userPickedMode = ref(false);

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
    if (!userPickedMode.value && isXciInput(inputPath)) {
      mode.value = "block";
    }
  }

  function removeFromQueue(id: string) {
    queue.value = queue.value.filter((item) => item.id !== id);
  }

  function clearQueue() {
    queue.value = [];
  }

  function setMode(m: NxMode) {
    mode.value = m;
    userPickedMode.value = true;
  }

  function $reset() {
    queue.value = [];
    recursive.value = true;
    maxDepth.value = null;
    output.value = "";
    keys.value = "";
    level.value = 18;
    mode.value = "solid";
    blockSizeExp.value = 20;
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    reportFile.value = "";
    userPickedMode.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    queue,
    recursive,
    maxDepth,
    output,
    keys,
    level,
    mode,
    blockSizeExp,
    onConflict,
    skipSpaceCheck,
    outputTemplate,
    reportFile,
    result,
    error,
    loading,
    addToQueue,
    removeFromQueue,
    clearQueue,
    setMode,
    $reset,
  };
});
