import type { Ref } from "vue";
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { BatchItem } from "~/types/batch";

export function useBatchOperation(
  taskId: string,
  commandName: string,
  buildArgs: (item: BatchItem) => Record<string, unknown>,
) {
  const running = ref(false);
  const currentIndex = ref(-1);
  const aborted = ref(false);

  const progress = useProgress(taskId);

  async function start(queue: Ref<BatchItem[]>, resultRef: Ref<string>) {
    running.value = true;
    aborted.value = false;
    currentIndex.value = 0;

    let doneCount = 0;
    let errorCount = 0;

    for (let i = 0; i < queue.value.length; i++) {
      if (aborted.value) break;

      const item = queue.value[i];
      if (item.status === "done" || item.status === "error") {
        if (item.status === "done") doneCount++;
        if (item.status === "error") errorCount++;
        continue;
      }

      currentIndex.value = i;
      item.status = "running";
      progress.reset();

      try {
        const res = await invoke<string>(commandName, buildArgs(item));
        item.status = "done";
        item.result = res;
        doneCount++;
      } catch (e) {
        item.status = "error";
        item.error = String(e);
        errorCount++;
      }
    }

    running.value = false;
    currentIndex.value = -1;

    const total = queue.value.length;
    if (errorCount === 0) {
      resultRef.value = `All ${total} files processed successfully`;
    } else {
      resultRef.value = `${doneCount} of ${total} succeeded, ${errorCount} failed`;
    }
  }

  function abort() {
    aborted.value = true;
  }

  return { running, currentIndex, progress, start, abort };
}
