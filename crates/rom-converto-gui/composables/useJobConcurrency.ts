import { ref, watch } from "vue";

const STORAGE_KEY = "rom-converto:job-concurrency";
const DEFAULT_CONCURRENCY = 2;
const MAX_CONCURRENCY = 8;

function readInitial(): number {
  try {
    const raw = Number(localStorage.getItem(STORAGE_KEY));
    if (Number.isInteger(raw) && raw >= 1 && raw <= MAX_CONCURRENCY) return raw;
  } catch {
    // localStorage unavailable; fall through to the default.
  }
  return DEFAULT_CONCURRENCY;
}

// Module-level so every page shares the same preference instance.
const concurrency = ref(readInitial());

watch(concurrency, (v) => {
  if (!Number.isInteger(v) || v < 1 || v > MAX_CONCURRENCY) {
    concurrency.value = Math.min(Math.max(1, Math.trunc(v) || 1), MAX_CONCURRENCY);
    return;
  }
  try {
    localStorage.setItem(STORAGE_KEY, String(v));
  } catch {
    // localStorage unavailable; preference just won't persist across restarts.
  }
});

export function useJobConcurrency() {
  return { concurrency, maxConcurrency: MAX_CONCURRENCY };
}
