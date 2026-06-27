<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useNxVerifyStore, type NxVerifyResult } from "~/stores/nx-verify";

const store = useNxVerifyStore();
const { input, keys, verdict, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("nx-verify");

const isBatch = computed(() => queue.value.length > 0);

const CONTAINER_FILTERS = [
  { name: "Switch container", extensions: ["nsp", "nsz", "xci", "xcz"] },
];

const commandLine = ref("");

function verifyArgs(inputPath: string) {
  return { input: inputPath, keys: keys.value || null };
}

const batch = useBatchOperation("nx-verify", "cmd_nx_verify", (item) =>
  verifyArgs(item.input),
);

function handleFiles(paths: string[]) {
  for (const p of paths) store.addToQueue(p);
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path);
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  verdict.value = null;
  error.value = "";
  result.value = "";

  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_nx_verify", verifyArgs(rep.input)) : "";
    await batch.start(queue, result, {
      isSuccess: (res) => {
        try {
          return JSON.parse(res).ok !== false;
        } catch {
          return true;
        }
      },
      failureMessage: (res) => {
        try {
          const v = JSON.parse(res) as NxVerifyResult;
          const bad = v.ncas.filter((n) => !n.ok).length;
          return `verification failed (${bad} NCA${bad === 1 ? "" : "s"} with mismatches)`;
        } catch {
          return "verification failed";
        }
      },
    });
  } else {
    const args = verifyArgs(input.value);
    commandLine.value = buildCliCommand("cmd_nx_verify", args);
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_nx_verify", args);
      verdict.value = JSON.parse(json) as NxVerifyResult;
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
      title="Verify integrity"
      description="Decrypt every NCA section in a Switch container and recompute the FsHeader's stored chunk hashes. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="(!!verdict && verdict.ok) || !!result"
      :has-error="!!error || (!!verdict && verdict.ok === false)"
    />

    <OperationCard>
      <div class="space-y-5">
        <template v-if="isBatch">
          <BatchFileList
            :items="queue"
            :current-index="batch.currentIndex.value"
            :running="batch.running.value"
            :progress="batch.progress"
            @remove="store.removeFromQueue"
            @clear="store.clearQueue"
          />

          <FileDropZone
            label="Add more containers"
            model-value=""
            :multiple="true"
            :filters="CONTAINER_FILTERS"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <FileDropZone
          v-else
          :model-value="input"
          label="Input container"
          :multiple="true"
          :filters="CONTAINER_FILTERS"
          :primary="true"
          @update:model-value="handleSingleFile"
          @update:files="handleFiles"
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

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Verify All (${queue.filter(i => i.status === 'pending').length})` : 'Verify' }}
        </RunButton>

        <div v-if="!isBatch && verdict" class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <div class="flex items-center justify-between">
            <div>
              <div class="text-sm font-medium text-zinc-200">{{ verdict.kind }}</div>
              <div class="text-xs text-zinc-400">
                {{ verdict.ncas.length }} NCA{{ verdict.ncas.length === 1 ? "" : "s" }} verified
              </div>
            </div>
            <span
              class="rounded-full px-2.5 py-0.5 text-xs font-semibold"
              :class="verdict.ok ? 'bg-emerald-500/15 text-emerald-300' : 'bg-red-500/20 text-red-300'"
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
                :class="nca.ok ? 'text-emerald-400' : 'text-red-400'"
              >
                {{ nca.ok ? "OK" : `${nca.mismatched_sections} bad` }}
              </span>
            </li>
          </ul>
        </div>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="isBatch ? result : ''" :error="error" />
    </div>
  </div>
</template>
