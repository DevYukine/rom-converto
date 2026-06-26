<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useChdVerifyStore } from "~/stores/chd-verify";

const store = useChdVerifyStore();
const { input, parent, fix, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("chd-verify");

const isBatch = computed(() => queue.value.length > 0);

function verdictPassed(res: string) {
  try {
    return JSON.parse(res).ok !== false;
  } catch {
    return true;
  }
}

const batch = useBatchOperation("chd-verify", "cmd_chd_verify", (item) => ({
  input: item.input,
  parent: parent.value || null,
  fix: fix.value,
}));

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
    await batch.start(queue, result, {
      isSuccess: verdictPassed,
      failureMessage: () => "verification failed",
    });
  } else {
    await run("cmd_chd_verify", {
      input: input.value,
      parent: parent.value || null,
      fix: fix.value,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Verify CHD"
      description="Verify CHD file integrity by checking SHA-1 hashes. Drop multiple .chd files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result && (isBatch || verdictPassed(result))"
      :has-error="!!error || (!isBatch && !!result && !verdictPassed(result))"
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
            label="Add more CHD files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <FileDropZone
          v-else
          :model-value="input"
          label="Input CHD file"
          :multiple="true"
          :filters="[{ name: 'CHD', extensions: ['chd'] }]"
          :primary="true"
          @update:model-value="handleSingleFile"
          @update:files="handleFiles"
        />

        <FileDropZone
          v-model="parent"
          label="Parent CHD (optional)"
          :filters="[{ name: 'CHD', extensions: ['chd'] }]"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="fix"
            label="Fix SHA-1"
            description="Automatically fix incorrect SHA-1 values in the header"
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
          {{ isBatch ? `Verify ${queue.filter(i => i.status === 'pending').length} files` : 'Verify' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
