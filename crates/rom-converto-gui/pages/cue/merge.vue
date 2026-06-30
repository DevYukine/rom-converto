<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCueMergeStore } from "~/stores/cue-merge";

const store = useCueMergeStore();
const { input, output, onConflict, skipSpaceCheck, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cue-merge");
const commandLine = ref("");

const previewMode = ref(false);
const { preview, single: previewSingle, error: previewError } = usePreview("cmd_cue_merge");

const { canRun, runBlockReason } = usePageGating({ input, emptyInputReason: "Select an input file to continue." });

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveMergedCuePath(input.value));
});

function mergeArgs() {
  return {
    cuePath: input.value,
    output: output.value || resolve(deriveMergedCuePath(input.value)),
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    dryRun: previewMode.value,
  };
}

async function execute() {
  progress.reset();
  const args = mergeArgs();
  commandLine.value = buildCliCommand("cmd_cue_merge", args);
  await run("cmd_cue_merge", args);
}

async function runPreview() {
  const args = mergeArgs();
  commandLine.value = buildCliCommand("cmd_cue_merge", args);
  await previewSingle(args);
  if (previewError.value) error.value = previewError.value;
}

function onRun() {
  if (previewMode.value) runPreview();
  else execute();
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
          <FlagToggle
            v-model="skipSpaceCheck"
            label="Skip free space check"
            description="Proceed even if the output filesystem looks too full to hold the result."
          />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <FlagToggle
          v-model="previewMode"
          label="Preview (dry run)"
          description="Show what would happen without writing anything."
        />

        <RunButton :loading="loading" :disabled="!canRun" :disabled-reason="runBlockReason" @click="onRun">
          {{ previewMode ? 'Preview' : 'Merge' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :error="error" />
    </div>
  </div>
</template>
