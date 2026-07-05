import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { DatDefaults, Preset, UserConfig } from "~/types/config";

// Backs the Settings page and every page's preset picker with the same
// `rom-converto.toml` presets the CLI reads. `activePreset` is the one
// selected globally; pages watch it via `usePreset` to apply the values
// covered by their own store fields.
export const useConfigStore = defineStore("config", () => {
  const configPath = ref<string | null>(null);
  const presets = ref<Record<string, Preset>>({});
  const dat = ref<DatDefaults | null>(null);
  const activePreset = ref<string | null>(null);
  const loaded = ref(false);
  const error = ref("");
  // Unused by settings logic; present only so the sidebar's generic
  // status-dot lookup (which expects loading/result/error) can include it.
  const loading = ref(false);
  const result = ref("");

  async function loadConfig() {
    if (loading.value || loaded.value) return;
    loading.value = true;
    try {
      const [path, cfg] = await Promise.all([
        invoke<string | null>("cmd_config_path"),
        invoke<UserConfig>("cmd_load_config"),
      ]);
      configPath.value = path;
      presets.value = cfg.presets ?? {};
      dat.value = cfg.dat ?? null;
      loaded.value = true;
      error.value = "";
    } catch (e) {
      error.value = String(e);
    } finally {
      loading.value = false;
    }
  }

  async function savePreset(name: string, preset: Preset) {
    try {
      await invoke("cmd_save_preset", { name, preset });
      presets.value = { ...presets.value, [name]: preset };
      configPath.value = await invoke<string | null>("cmd_config_path");
      error.value = "";
    } catch (e) {
      error.value = String(e);
      throw e;
    }
  }

  async function deletePreset(name: string) {
    try {
      await invoke("cmd_delete_preset", { name });
      const next = { ...presets.value };
      delete next[name];
      presets.value = next;
      if (activePreset.value === name) activePreset.value = null;
      error.value = "";
    } catch (e) {
      error.value = String(e);
      throw e;
    }
  }

  function applyPreset(name: string | null) {
    activePreset.value = name || null;
  }

  return {
    configPath,
    presets,
    dat,
    activePreset,
    loaded,
    error,
    loading,
    result,
    loadConfig,
    savePreset,
    deletePreset,
    applyPreset,
  };
});
