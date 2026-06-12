<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useDolVerifyStore, type DolVerifyResult } from "~/stores/dol-verify";

const store = useDolVerifyStore();
const { input, full, verdict, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("dol-verify");

const isBatch = computed(() => queue.value.length > 0);

const DISC_FILTERS = [
  { name: "GameCube disc", extensions: ["iso", "gcm", "rvz"] },
];

const batch = useBatchOperation("dol-verify", "cmd_verify_dol", (item) => ({
  input: item.input,
  full: full.value,
}));

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
    await batch.start(queue, result);
  } else {
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_verify_dol", {
        input: input.value,
        full: full.value,
      });
      verdict.value = JSON.parse(json) as DolVerifyResult;
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
      description="Check the RVZ container hashes of a GameCube disc, or run a full whole-disc digest. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!verdict || !!result"
      :has-error="!!error"
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
            description="Decode the whole disc and compute a whole-disc SHA-1. GameCube discs carry no built-in hashes, so this digest is informational for DAT matching, not a pass or fail."
          />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch ? `Verify ${queue.filter(i => i.status === 'pending').length} Files` : 'Verify' }}
        </RunButton>

        <div v-if="!isBatch && verdict" class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <div class="flex items-center justify-between">
            <div>
              <div class="text-sm font-medium text-zinc-200">{{ verdict.game_id }}</div>
              <div v-if="verdict.disc_sha1" class="mt-0.5 break-all font-mono text-xs text-zinc-400">
                SHA-1 {{ verdict.disc_sha1 }}
              </div>
            </div>
            <span
              class="ml-3 shrink-0 rounded-full px-2.5 py-0.5 text-xs font-semibold"
              :class="verdict.ok ? 'bg-emerald-500/15 text-emerald-300' : 'bg-rose-500/15 text-rose-300'"
            >
              {{ verdict.ok ? "OK" : "FAIL" }}
            </span>
          </div>

          <ul v-if="verdict.structural?.notes.length" class="mt-3 space-y-1">
            <li
              v-for="(note, i) in verdict.structural.notes"
              :key="i"
              class="rounded-md bg-zinc-900/60 px-3 py-1.5 text-xs text-zinc-400"
            >
              {{ note }}
            </li>
          </ul>
        </div>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="isBatch ? result : ''" :error="error" />
    </div>
  </div>
</template>
