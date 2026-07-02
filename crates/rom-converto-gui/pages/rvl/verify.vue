<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useRvlVerifyStore, type RvlVerifyResult } from "~/stores/rvl-verify";

const store = useRvlVerifyStore();
const { input, full, verdict, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("rvl-verify");

const isBatch = computed(() => queue.value.length > 0);

const DISC_FILTERS = [
  { name: "Wii disc", extensions: ["iso", "wbfs", "rvz"] },
];

const commandLine = ref("");

function verifyArgs(inputPath: string) {
  return { input: inputPath, full: full.value };
}

const batch = useBatchOperation("rvl-verify", "cmd_verify_rvl", (item) =>
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

function partitionLabel(offset: number): string {
  return `0x${offset.toString(16).toUpperCase()}`;
}

async function execute() {
  progress.reset();
  verdict.value = null;
  error.value = "";
  result.value = "";

  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_verify_rvl", verifyArgs(rep.input)) : "";
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
          const v = JSON.parse(res) as RvlVerifyResult;
          const mismatches = v.partitions.reduce((sum, p) => sum + p.mismatched_clusters, 0);
          return `verification failed (${mismatches} corrupt cluster${mismatches === 1 ? "" : "s"})`;
        } catch {
          return "verification failed";
        }
      },
    });
  } else {
    const args = verifyArgs(input.value);
    commandLine.value = buildCliCommand("cmd_verify_rvl", args);
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_verify_rvl", args);
      verdict.value = JSON.parse(json) as RvlVerifyResult;
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
      title="Verify Wii disc"
      description="Check the RVZ container hashes of a Wii disc, or recompute the full partition hash tree. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="(!!verdict && verdict.ok) || !!result"
      :has-error="!!error || (!!verdict && verdict.ok === false)"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="isBatch ? result : ''" :error="error" />
    </div>

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
            label="Add more discs"
            model-value=""
            :multiple="true"
            :filters="DISC_FILTERS"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <FileDropZone
          v-else
          :model-value="input"
          label="Input disc"
          :multiple="true"
          :filters="DISC_FILTERS"
          :primary="true"
          @update:model-value="handleSingleFile"
          @update:files="handleFiles"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="full"
            label="Full verification"
            description="Decrypt every partition cluster and recompute the H0/H1/H2 hash tree to detect tampering or bit rot. Hashes the entire disc and can be slow."
          />
        </div>

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
            <div class="text-sm font-medium text-zinc-200">{{ verdict.game_id }}</div>
            <span
              class="ml-3 shrink-0 rounded-full px-2.5 py-0.5 text-xs font-semibold"
              :class="verdict.ok ? 'bg-emerald-500/15 text-emerald-300' : 'bg-red-500/15 text-red-300'"
            >
              {{ verdict.ok ? "OK" : "FAIL" }}
            </span>
          </div>

          <ul v-if="verdict.partitions.length" class="mt-3 space-y-1">
            <li
              v-for="p in verdict.partitions"
              :key="p.offset"
              class="rounded-md bg-zinc-900/60 px-3 py-1.5 text-xs"
            >
              <div class="flex items-center justify-between">
                <div class="flex items-center gap-2 truncate">
                  <span class="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] font-medium text-zinc-400">
                    {{ p.kind }}
                  </span>
                  <span class="truncate font-mono text-zinc-200">@{{ partitionLabel(p.offset) }}</span>
                </div>
                <span
                  class="ml-3 shrink-0 font-semibold"
                  :class="p.ok ? 'text-emerald-400' : 'text-red-400'"
                >
                  {{ p.ok ? "OK" : `${p.mismatched_clusters} corrupt` }}
                </span>
              </div>
              <div v-if="p.scrubbed_clusters > 0" class="mt-0.5 text-[11px] text-zinc-500">
                {{ p.scrubbed_clusters }} scrubbed cluster{{ p.scrubbed_clusters === 1 ? "" : "s" }} skipped
              </div>
            </li>
          </ul>
        </div>
      </div>
    </OperationCard>
  </div>
</template>
