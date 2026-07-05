<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { useWupVerifyStore, type WupVerifyResult } from "~/stores/wup-verify";

const store = useWupVerifyStore();
const { input, keys, verdict, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("wup-verify");

const isBatch = computed(() => queue.value.length > 0);

const DISC_FILTERS = [{ name: "Wii U disc images", extensions: ["wud", "wux", "zip", "7z", "rar", "tar", "tgz", "gz"] }];

const commandLine = ref("");

function verifyArgs(inputPath: string) {
  return { input: inputPath, keys: keys.value || null };
}

const batch = useBatchOperation("wup-verify", "cmd_wup_verify", (item) =>
  verifyArgs(item.input),
);

function handleReorder(ids: string[]) {
  batch.reorder(queue, ids);
}

function handleRemoveSelected(ids: string[]) {
  batch.removeSelected(queue, ids);
}

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

function addPaths(paths: string[]) {
  for (const p of paths) {
    if (!p) continue;
    if (queue.value.length > 0 || (input.value && paths.length > 1)) {
      store.addToQueue(p);
    } else if (input.value) {
      store.addToQueue(input.value);
      store.addToQueue(p);
      input.value = "";
    } else {
      input.value = p;
    }
  }
}

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(dropZoneRef.value, addPaths, 0);
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

async function browseDirectory() {
  const picked = await open({ directory: true, multiple: true });
  if (!picked) return;
  addPaths(Array.isArray(picked) ? picked : [picked]);
}

async function browseDiscImage() {
  const picked = await open({ directory: false, multiple: true, filters: DISC_FILTERS });
  if (!picked) return;
  addPaths(Array.isArray(picked) ? picked : [picked]);
}

async function execute() {
  progress.reset();
  verdict.value = null;
  error.value = "";
  result.value = "";

  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_wup_verify", verifyArgs(rep.input)) : "";
    await batch.start(queue, result, {
      errorRef: error,
      isSuccess: (res) => {
        try {
          return JSON.parse(res).ok !== false;
        } catch {
          return true;
        }
      },
      failureMessage: (res) => {
        try {
          const v = JSON.parse(res) as WupVerifyResult;
          const mismatches = v.titles.reduce((sum, t) => sum + t.mismatched_content, 0);
          return `verification failed (${mismatches} mismatched content)`;
        } catch {
          return "verification failed";
        }
      },
    });
  } else {
    const args = verifyArgs(input.value);
    commandLine.value = buildCliCommand("cmd_wup_verify", args);
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_wup_verify", args);
      verdict.value = JSON.parse(json) as WupVerifyResult;
    } catch (e) {
      error.value = String(e);
    } finally {
      loading.value = false;
    }
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Verify Wii U title"
      description="Verify a Wii U title against its TMD hashes. Inputs can be NUS or loadiine directories, .wua archives, or .wud / .wux disc images. Drop multiple inputs for batch processing."
      :loading="loading || batch.running.value"
      :has-result="(!!verdict && verdict.ok) || !!result"
      :has-error="!!error || (!!verdict && verdict.ok === false)"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="isBatch ? result : ''" :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <BatchFileList
          v-if="isBatch"
          :items="queue"
          :running="batch.running.value"
          :progress-slots="batch.progressSlots"
          @remove="store.removeFromQueue"
          @clear="store.clearQueue"
          @reorder="handleReorder"
          @remove-selected="handleRemoveSelected"
          @retry-failed="execute"
        />

        <div v-else-if="input" class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">Input</label>
          <div class="flex items-center justify-between rounded-lg border border-zinc-700 bg-zinc-800/30 px-4 py-3">
            <span class="truncate text-sm text-zinc-200" :title="input">{{ input.split(/[\\/]/).pop() }}</span>
            <button
              type="button"
              class="ml-3 shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-400 transition hover:bg-zinc-700 hover:text-zinc-200"
              @click="input = ''"
            >
              Clear
            </button>
          </div>
        </div>

        <div class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">
            {{ isBatch || input ? "Add more inputs" : "Input" }}
          </label>
          <div
            ref="dropZoneRef"
            class="drop-zone flex cursor-default flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-zinc-700 bg-zinc-800/30 px-4 py-6 transition xl:py-8"
          >
            <svg class="h-8 w-8 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
              <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
            </svg>
            <span class="text-center text-sm text-zinc-400">
              Drop a NUS or loadiine folder, a .wua archive, or a .wud / .wux disc image.
            </span>
            <div class="flex flex-wrap items-center justify-center gap-2 pt-1">
              <button
                type="button"
                class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
                @click="browseDirectory"
              >
                Browse folder
              </button>
              <button
                type="button"
                class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
                @click="browseDiscImage"
              >
                Browse disc image
              </button>
            </div>
          </div>
        </div>

        <FileDropZone
          v-model="keys"
          label="Disc master key file (optional, for .wud / .wux inputs only)"
          :filters="[{ name: 'Disc key', extensions: ['key', 'bin', 'txt'] }]"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Verify all (${queue.filter(i => i.status === 'pending').length})` : 'Verify' }}
        </RunButton>

        <div v-if="!isBatch && verdict" class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <div class="flex items-center justify-between">
            <div>
              <div class="text-sm font-medium text-zinc-200">{{ verdict.kind }}</div>
              <div class="text-xs text-zinc-400">
                {{ verdict.titles.length }} title{{ verdict.titles.length === 1 ? "" : "s" }} checked
              </div>
            </div>
            <span
              class="ml-3 shrink-0 rounded-full px-2.5 py-0.5 text-xs font-semibold"
              :class="verdict.ok ? 'bg-emerald-500/15 text-emerald-300' : 'bg-red-500/15 text-red-300'"
            >
              {{ verdict.ok ? "OK" : "FAIL" }}
            </span>
          </div>

          <ul v-if="verdict.titles.length" class="mt-3 space-y-1">
            <li
              v-for="t in verdict.titles"
              :key="t.title_id_hex"
              class="rounded-md bg-zinc-900/60 px-3 py-1.5 text-xs"
            >
              <div class="flex items-center justify-between">
                <span class="truncate font-mono text-zinc-200">{{ t.title_id_hex }}</span>
                <span
                  class="ml-3 shrink-0 font-semibold"
                  :class="t.ok ? 'text-emerald-400' : 'text-red-400'"
                >
                  {{ t.ok ? "OK" : "FAIL" }}
                </span>
              </div>
              <div class="mt-0.5 text-[11px] text-zinc-500">
                verified {{ t.verified_content }}, mismatched {{ t.mismatched_content }}, skipped {{ t.skipped_content }}
              </div>
            </li>
          </ul>
        </div>
      </div>
    </OperationCard>
  </div>
</template>

<style scoped>
.drop-zone.drop-hover {
  border-color: rgb(14 165 233);
  background-color: rgb(14 165 233 / 0.08);
  box-shadow: 0 0 0 1px rgb(14 165 233 / 0.3);
}
</style>
