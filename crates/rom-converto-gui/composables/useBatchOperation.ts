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

  async function start(
    queue: Ref<BatchItem[]>,
    resultRef: Ref<string>,
    options?: {
      isSuccess?: (result: string) => boolean;
      failureMessage?: (result: string) => string;
    },
  ) {
    running.value = true;
    aborted.value = false;
    currentIndex.value = 0;

    let doneCount = 0;
    let errorCount = 0;
    let cancelledCount = 0;

    for (let i = 0; i < queue.value.length; i++) {
      if (aborted.value) break;

      // noUncheckedIndexedAccess widens `queue[i]` to `T | undefined`.
      // Loop bound rules it out, but skip defensively.
      const item = queue.value[i];
      if (!item) continue;
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
        if (options?.isSuccess && !options.isSuccess(res)) {
          item.status = "error";
          item.result = res;
          item.error = options.failureMessage
            ? options.failureMessage(res)
            : "verification failed";
          errorCount++;
        } else {
          item.status = "done";
          item.result = res;
          doneCount++;
        }
      } catch (e) {
        const message = String(e);
        if (message.includes("operation cancelled")) {
          item.status = "cancelled";
          item.error = message;
          cancelledCount++;
          break;
        }
        item.status = "error";
        item.error = message;
        errorCount++;
      }
    }

    running.value = false;
    currentIndex.value = -1;

    const total = queue.value.length;
    if (errorCount === 0 && cancelledCount === 0) {
      resultRef.value = `All ${total} files processed successfully`;
    } else {
      let summary = `${doneCount} of ${total} succeeded`;
      if (errorCount > 0) summary += `, ${errorCount} failed`;
      if (cancelledCount > 0) summary += `, ${cancelledCount} cancelled`;
      resultRef.value = summary;
    }
  }

  async function abort() {
    aborted.value = true;
    try {
      await invoke("cmd_cancel");
    } catch {
      // The running task may have already finished; nothing to cancel.
    }
  }

  return { running, currentIndex, progress, start, abort };
}
