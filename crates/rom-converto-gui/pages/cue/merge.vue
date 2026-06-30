<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCueMergeStore } from "~/stores/cue-merge";

const store = useCueMergeStore();
const { input, output, onConflict, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cue-merge");
const commandLine = ref("");

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveMergedCuePath(input.value));
});

async function execute() {
  progress.reset();
  const args = {
    cuePath: input.value,
    output: output.value || resolve(deriveMergedCuePath(input.value)),
    onConflict: onConflict.value,
  };
  commandLine.value = buildCliCommand("cmd_cue_merge", args);
  await run("cmd_cue_merge", args);
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
            label="Input CUE file (multi-bin)"
            :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
            :primary="true"
          />

          <FileDropZone
            v-model="output"
            label="Output CUE file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: 'CUE Sheet', extensions: ['cue'] }]"
          />
        </div>

        <OutputDirField v-model="outputDir" />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <ConflictPolicyControl v-model="onConflict" />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!input" @click="execute">
          Merge
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
