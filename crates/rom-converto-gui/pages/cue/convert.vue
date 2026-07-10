<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCueConvertStore } from "~/stores/cue-convert";

const store = useCueConvertStore();
const { input, output, format, onConflict, skipSpaceCheck, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });

const commandLine = ref("");
const previewMode = ref(false);

const FORMAT_OPTIONS = [
  { label: "ISO", value: "iso" },
  { label: "CSO (PSP, PPSSPP)", value: "cso" },
  { label: "ZSO (PS2 via OPL)", value: "zso" },
];

// ISO goes through cue_to_iso; CSO and ZSO share cue_to_cso, which takes the
// container format as an argument.
const command = computed(() => (format.value === "iso" ? "cmd_cue_to_iso" : "cmd_cue_to_cso"));

const progressIso = useProgress("cue-to-iso");
const progressCso = useProgress("cue-to-cso");
const progress = computed(() => (format.value === "iso" ? progressIso : progressCso));

const previewIso = usePreview("cmd_cue_to_iso");
const previewCso = usePreview("cmd_cue_to_cso");
const activePreview = computed(() => (format.value === "iso" ? previewIso : previewCso));

const { canRun, runBlockReason } = usePageGating({ input, emptyInputReason: "Select an input file to continue." });

function deriveOutput(cue: string, fmt: "iso" | "cso" | "zso"): string {
  return fmt === "iso" ? deriveDiscIsoPath(cue) : deriveCsoPath(cue, fmt);
}

watch([input, outputDir, format], () => {
  if (input.value) output.value = resolve(deriveOutput(input.value, format.value));
});

function convertArgs() {
  return {
    cuePath: input.value,
    output: output.value || resolve(deriveOutput(input.value, format.value)),
    format: format.value,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    dryRun: previewMode.value,
  };
}

async function execute() {
  progress.value.reset();
  const args = convertArgs();
  commandLine.value = buildCliCommand(command.value, args);
  await run(command.value, args);
}

async function runPreview() {
  const args = convertArgs();
  commandLine.value = buildCliCommand(command.value, args);
  await activePreview.value.single(args);
  if (activePreview.value.error.value) error.value = activePreview.value.error.value;
}

function onRun() {
  if (previewMode.value) runPreview();
  else execute();
}
</script>

<template>
  <div>
    <PageHeader
      title="Convert to ISO/CSO/ZSO"
      description="Convert a .cue/.bin disc image's data track to a plain .iso, or straight to a block-compressed .cso or .zso. Audio tracks are skipped."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :preview="activePreview.preview.value" :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <div class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            v-model="input"
            label="Input CUE file"
            :filters="[{ name: 'CUE Sheet', extensions: ['cue', 'zip', '7z', 'rar', 'tar', 'tgz', 'gz'] }]"
            :primary="true"
          />

          <FileDropZone
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: 'Disc image', extensions: ['iso', 'cso', 'zso'] }]"
          />
        </div>

        <OutputDirField v-model="outputDir" />

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <SegmentedControl
            :model-value="format"
            label="Format"
            :options="FORMAT_OPTIONS"
            @update:model-value="(v: string) => { format = v as 'iso' | 'cso' | 'zso' }"
          />
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
          {{ previewMode ? 'Preview' : 'Convert' }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>
