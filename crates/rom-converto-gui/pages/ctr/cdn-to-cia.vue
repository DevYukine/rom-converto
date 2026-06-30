<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrCdnToCiaStore } from "~/stores/ctr-cdn-to-cia";

const store = useCtrCdnToCiaStore();
const { cdnDir, output, decrypt, compress, cleanup, recursive, ensureTicket, onConflict, skipSpaceCheck, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("cdn-to-cia");
const totalProgress = useProgress("cdn-to-cia-total");
const commandLine = ref("");

watch(compress, (val) => {
  if (val) decrypt.value = true;
});

function deriveCiaPath(dir: string): string {
  const trimmed = dir.replace(/[\\/]+$/, "");
  return `${trimmed}.cia`;
}

async function execute() {
  progress.reset();
  totalProgress.reset();
  const target = output.value || (outputDir.value ? resolve(deriveCiaPath(cdnDir.value)) : null);
  const args = {
    cdnDir: cdnDir.value,
    output: target,
    decrypt: decrypt.value,
    compress: compress.value,
    cleanup: cleanup.value,
    recursive: recursive.value,
    ensureTicketExists: ensureTicket.value,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
  };
  commandLine.value = buildCliCommand("cmd_cdn_to_cia", args);
  await run("cmd_cdn_to_cia", args);
}
</script>

<template>
  <div>
    <PageHeader
      title="CDN to CIA"
      description="Convert Nintendo CDN content to CIA format."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <div class="grid gap-5 lg:grid-cols-2">
          <div class="space-y-5">
            <FileDropZone
              v-model="cdnDir"
              label="CDN directory"
              :directory="true"
              :primary="true"
            />

            <FileDropZone
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'CIA', extensions: ['cia'] }]"
            />
          </div>

          <div class="space-y-1 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
            <FlagToggle
              v-model="decrypt"
              label="Decrypt"
              description="Decrypt the CIA after conversion"
              :disabled="compress"
            />
            <FlagToggle
              v-model="compress"
              label="Compress"
              description="Compress to .zcia after conversion (requires decrypt)"
            />
            <FlagToggle
              v-model="ensureTicket"
              label="Generate ticket"
              description="Generate a ticket if missing"
            />
            <FlagToggle
              v-model="recursive"
              label="Recursive"
              description="Process all subdirectories"
            />
            <FlagToggle
              v-model="cleanup"
              label="Cleanup"
              description="Delete CDN files after conversion"
            />
          </div>
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <ConflictPolicyControl v-model="onConflict" />
          <FlagToggle
            v-model="skipSpaceCheck"
            label="Skip free space check"
            description="Proceed even if the output filesystem looks too full to hold the result."
          />
        </div>

        <OutputDirField v-model="outputDir" />

        <ProgressBar
          v-if="recursive"
          :percent="totalProgress.percent.value"
          :message="totalProgress.message.value"
          :running="totalProgress.running.value"
        />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!cdnDir" @click="execute" @cancel="abort()">
          Convert
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>
