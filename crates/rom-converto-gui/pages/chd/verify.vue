<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useChdVerifyStore } from "~/stores/chd-verify";

const store = useChdVerifyStore();
const { input, parent, fix, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("chd-verify");

const isBatch = computed(() => queue.value.length > 0);

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
    await batch.start(queue, result);
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
      description="Verify CHD file integrity by checking SHA1 hashes. Drop multiple .chd files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <!-- Batch mode -->
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

        <!-- Single mode: 2-col on large screens -->
        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input CHD File"
            :multiple="true"
            :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <div class="space-y-5">
            <FileDropZone
              v-model="parent"
              label="Parent CHD (optional)"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            />

            <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
              <FlagToggle
                v-model="fix"
                label="Fix SHA1"
                description="Automatically fix incorrect SHA1 values in the header"
              />
            </div>
          </div>
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
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
