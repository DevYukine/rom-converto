import { watch, type Ref } from "vue";
import { useConfigStore } from "~/stores/config";
import type { PresetFormat } from "~/types/config";

/** Config key (e.g. `chunk_size`) to the page's own store ref holding it. */
type PresetBindings = Record<string, Ref<string | number | null>>;

/**
 * Applies the active preset's table for `format` into `bindings` on mount
 * and whenever the active preset changes, and lets the page save its
 * current option values as a new (or replacement) preset. Only keys present
 * in `bindings` are read or written, mirroring the CLI's config exclusions
 * (recursive/max-depth/output-template stay per-page).
 */
export function usePreset(format: PresetFormat, bindings: PresetBindings) {
  const store = useConfigStore();
  if (!store.loaded) store.loadConfig();

  async function saveAs(name: string) {
    const table: Record<string, string | number> = {};
    for (const [key, ref] of Object.entries(bindings)) {
      const value = ref.value;
      if (value !== null && value !== undefined && value !== "") {
        table[key] = value;
      }
    }
    const preset = { ...store.presets[name], [format]: table };
    await store.savePreset(name, preset);
  }

  function apply(name: string | null) {
    if (!name) return;
    const table = store.presets[name]?.[format];
    if (!table) return;
    for (const [key, ref] of Object.entries(bindings)) {
      const value = (table as Record<string, string | number | null | undefined>)[key];
      if (value !== null && value !== undefined) {
        ref.value = value;
      }
    }
  }

  watch(
    () => [store.activePreset, store.loaded] as const,
    ([name]) => apply(name),
    { immediate: true },
  );

  return { saveAs };
}
