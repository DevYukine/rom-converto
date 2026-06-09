<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCueMergeStore } from "~/stores/cue-merge";

const store = useCueMergeStore();
const { input, output, force, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cue-merge");

watch(input, (val) => {
  if (val) output.value = deriveMergedCuePath(val);
});

async function execute() {
  progress.reset();
  await run("cmd_cue_merge", {
    cuePath: input.value,
    output: output.value,
    force: force.value,
  });
}
</script>

<template>
  <div>
    <PageHeader
      title="Merge multi-bin"
      description="Merge a .cue referencing multiple .bin tracks into a single .bin + .cue pair for emulators that cannot load split images."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <div class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            v-model="input"
            label="Input CUE File (multi-bin)"
            :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
            :primary="true"
          />

          <FileDropZone
            v-model="output"
            label="Output CUE (auto-derived)"
            :save-dialog="true"
            :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
          />
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="force"
            label="Force Overwrite"
            description="Overwrite output files if they already exist"
          />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!input || !output" @click="execute">
          Merge
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
