<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useNxVerifyStore, type NxVerifyResult } from "~/stores/nx-verify";

const store = useNxVerifyStore();
const { input, keys, verdict, error, loading } = storeToRefs(store);
const progress = useProgress("nx-verify");

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

function addPaths(paths: string[]) {
  if (paths[0]) input.value = paths[0];
}

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(dropZoneRef.value, addPaths, 0);
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

async function execute() {
  progress.reset();
  verdict.value = null;
  error.value = "";
  loading.value = true;
  try {
    const json = await invoke<string>("cmd_nx_verify", {
      input: input.value,
      keys: keys.value || null,
    });
    verdict.value = JSON.parse(json) as NxVerifyResult;
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

const canVerify = computed(() => !!input.value);
</script>

<template>
  <div>
    <PageHeader
      title="Verify integrity"
      description="Decrypt every NCA section in a Switch container and recompute the FsHeader's stored chunk hashes."
      :loading="loading"
      :has-result="!!verdict"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="input"
          label="Input container"
          :filters="[{ name: 'Switch container', extensions: ['nsp', 'nsz', 'xci', 'xcz'] }]"
        />

        <FileDropZone
          v-model="keys"
          label="prod.keys (optional, falls back to ~/.switch/prod.keys)"
          :filters="[{ name: 'prod.keys', extensions: ['keys', 'txt'] }]"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!canVerify" @click="execute">
          Verify
        </RunButton>

        <div v-if="verdict" class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <div class="flex items-center justify-between">
            <div>
              <div class="text-sm font-medium text-zinc-200">{{ verdict.kind }}</div>
              <div class="text-xs text-zinc-400">
                {{ verdict.ncas.length }} NCA{{ verdict.ncas.length === 1 ? "" : "s" }} verified
              </div>
            </div>
            <span
              class="rounded-full px-2.5 py-0.5 text-xs font-semibold"
              :class="verdict.ok ? 'bg-emerald-500/15 text-emerald-300' : 'bg-rose-500/15 text-rose-300'"
            >
              {{ verdict.ok ? "OK" : "MISMATCHES" }}
            </span>
          </div>

          <ul v-if="verdict.ncas.length" class="mt-3 space-y-1">
            <li
              v-for="nca in verdict.ncas"
              :key="`${nca.partition ?? ''}|${nca.name}`"
              class="flex items-center justify-between rounded-md bg-zinc-900/60 px-3 py-1.5 text-xs"
            >
              <div class="flex items-center gap-2 truncate">
                <span
                  v-if="nca.partition"
                  class="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] font-medium text-zinc-400"
                >
                  {{ nca.partition }}
                </span>
                <span class="truncate font-mono text-zinc-200">{{ nca.name }}</span>
              </div>
              <span
                class="ml-3 shrink-0 font-semibold"
                :class="nca.ok ? 'text-emerald-400' : 'text-rose-400'"
              >
                {{ nca.ok ? "OK" : `${nca.mismatched_sections} bad` }}
              </span>
            </li>
          </ul>
        </div>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="''" :error="error" />
    </div>
  </div>
</template>
