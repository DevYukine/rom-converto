import { ref, computed, type ComputedRef, type Ref } from "vue";
import { listen } from "~/lib/ipc";

interface ProgressPayload {
  task_id: string;
  kind: "start" | "inc" | "finish" | "phase" | "warn";
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
  /** Advisory warnings; accumulate across a run (batch items share slots,
   *  which pass `keepWarnings` on their per-item resets) and stay visible
   *  after completion until the next run's full reset. */
  warnings: Ref<string[]>;
  percent: ComputedRef<number>;
  reset: (opts?: { keepWarnings?: boolean }) => void;
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
      case "phase":
        state.message.value = p.message;
        break;
      case "warn":
        if (!state.warnings.value.includes(p.message)) {
          state.warnings.value = [...state.warnings.value, p.message];
        }
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
  const warnings = ref<string[]>([]);

  const percent = computed(() =>
    total.value > 0 ? Math.round((current.value / total.value) * 100) : 0,
  );

  function reset(opts?: { keepWarnings?: boolean }) {
    total.value = 0;
    current.value = 0;
    message.value = "";
    running.value = false;
    if (!opts?.keepWarnings) warnings.value = [];
  }

  const state: ProgressState = { total, current, message, running, warnings, percent, reset };
  registry.set(taskId, state);
  return state;
}
