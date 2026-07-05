import { ref, watch } from "vue";

const STORAGE_KEY = "rom-converto:completion-sound";

function readInitial(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

// Module-level so every page shares the same preference instance.
const soundEnabled = ref(readInitial());

watch(soundEnabled, (v) => {
  try {
    localStorage.setItem(STORAGE_KEY, v ? "1" : "0");
  } catch {
    // localStorage unavailable; preference just won't persist across restarts.
  }
});

export function useNotifyPrefs() {
  return { soundEnabled };
}
