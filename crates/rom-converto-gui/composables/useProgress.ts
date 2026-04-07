import { ref, computed, onUnmounted } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface ProgressPayload {
  task_id: string;
  kind: "start" | "inc" | "finish";
  total: number;
  current: number;
  message: string;
}

export function useProgress(taskId: string) {
  const total = ref(0);
  const current = ref(0);
  const message = ref("");
  const running = ref(false);

  const percent = computed(() =>
    total.value > 0 ? Math.round((current.value / total.value) * 100) : 0,
  );

  let unlisten: UnlistenFn | null = null;

  listen<ProgressPayload>("progress", (event) => {
    const p = event.payload;
    if (p.task_id !== taskId) return;

    switch (p.kind) {
      case "start":
        total.value = p.total;
        current.value = 0;
        message.value = p.message;
        running.value = true;
        break;
      case "inc":
        current.value = p.current;
        break;
      case "finish":
        running.value = false;
        break;
    }
  }).then((fn) => {
    unlisten = fn;
  });

  onUnmounted(() => {
    unlisten?.();
  });

  function reset() {
    total.value = 0;
    current.value = 0;
    message.value = "";
    running.value = false;
  }

  return { total, current, message, running, percent, reset };
}
