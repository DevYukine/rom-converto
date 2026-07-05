<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoVerifyStore } from "~/stores/cso-verify";

const store = useCsoVerifyStore();
const { input, full, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cso-verify");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function csoArgs(inputPath: string) {
  return { inputPath, full: full.value };
}

const batch = useBatchOperation("cso-verify", "cmd_cso_verify", (item) =>
  csoArgs(item.input),
);

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p);
  }
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
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_cso_verify", csoArgs(rep.input)) : "";
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
          const v = JSON.parse(res);
          const count = typeof v.mismatches === "number" ? ` (${v.mismatches} mismatches)` : "";
          return `verification failed${count}`;
        } catch {
          return "verification failed";
        }
      },
    });
  } else {
    const args = csoArgs(input.value);
    commandLine.value = buildCliCommand("cmd_cso_verify", args);
    await run("cmd_cso_verify", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Verify CSO/ZSO/DAX"
      description="Check a CSO, ZSO, or DAX container: the structure is always validated; the full pass decodes every block. The formats embed no checksums. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
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
            label="Add more CSO/ZSO/DAX files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso', 'dax'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <FileDropZone
          v-else
          :model-value="input"
          label="Input CSO/ZSO/DAX file"
          :multiple="true"
          :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso', 'dax'] }]"
          :primary="true"
          @update:model-value="handleSingleFile"
          @update:files="handleFiles"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="full"
            label="Full verification"
            description="Decode every block instead of only checking the index"
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
      </div>
    </OperationCard>
  </div>
</template>
