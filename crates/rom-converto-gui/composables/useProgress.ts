import { ref, computed, type ComputedRef, type Ref } from "vue";
import { listen } from "@tauri-apps/api/event";

interface ProgressPayload {
  task_id: string;
  kind: "start" | "inc" | "finish";
  total: number;
  current: number;
  message: string;
}

// Concrete `Ref<T>` / `ComputedRef<T>` rather than
// `ReturnType<typeof ref<T>>`. The latter resolves to
// `Ref<T | undefined>` under the no-initial-value overload and
// would force every consumer to widen ProgressBar props.
interface ProgressState {
  total: Ref<number>;
  current: Ref<number>;
  message: Ref<string>;
  running: Ref<boolean>;
  percent: ComputedRef<number>;
  reset: () => void;
}

const registry = new Map<string, ProgressState>();
let listenerInitialized = false;

function initGlobalListener() {
  if (listenerInitialized) return;
  listenerInitialized = true;

  listen<ProgressPayload>("progress", (event) => {
    const p = event.payload;
    const state = registry.get(p.task_id);
    if (!state) return;

    switch (p.kind) {
      case "start":
        state.total.value = p.total;
        state.current.value = 0;
        state.message.value = p.message;
        state.running.value = true;
        break;
      case "inc":
        state.current.value = p.current;
        break;
      case "finish":
        state.running.value = false;
        break;
    }
  });
}

export function useProgress(taskId: string): ProgressState {
  const existing = registry.get(taskId);
  if (existing) return existing;

  initGlobalListener();

  const total = ref(0);
  const current = ref(0);
  const message = ref("");
  const running = ref(false);

  const percent = computed(() =>
    total.value > 0 ? Math.round((current.value / total.value) * 100) : 0,
  );

  function reset() {
    total.value = 0;
    current.value = 0;
    message.value = "";
    running.value = false;
  }

  const state: ProgressState = { total, current, message, running, percent, reset };
  registry.set(taskId, state);
  return state;
}
