<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrCdnToCiaStore } from "~/stores/ctr-cdn-to-cia";

const store = useCtrCdnToCiaStore();
const { cdnDir, output, decrypt, compress, cleanup, recursive, ensureTicket, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cdn-to-cia");
const totalProgress = useProgress("cdn-to-cia-total");

watch(compress, (val) => {
  if (val) decrypt.value = true;
});

async function execute() {
  progress.reset();
  totalProgress.reset();
  await run("cmd_cdn_to_cia", {
    cdnDir: cdnDir.value,
    output: output.value || null,
    decrypt: decrypt.value,
    compress: compress.value,
    cleanup: cleanup.value,
    recursive: recursive.value,
    ensureTicketExists: ensureTicket.value,
  });
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
        <!-- 2-col: files left, flags right -->
        <div class="grid gap-5 lg:grid-cols-2">
          <div class="space-y-5">
            <FileDropZone
              v-model="cdnDir"
              label="CDN Directory"
              :directory="true"
              :primary="true"
            />

            <FileDropZone
              v-model="output"
              label="Output (optional)"
              :save-dialog="true"
              :filters="[{ name: 'CIA', extensions: ['cia'] }]"
            />
          </div>

          <div class="space-y-1 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
            <FlagToggle
              v-model="decrypt"
              label="Decrypt"
              description="Decrypt the CIA after conversion"
            />
            <FlagToggle
              v-model="compress"
              label="Compress"
              description="Compress to .zcia after conversion (requires decrypt)"
            />
            <FlagToggle
              v-model="ensureTicket"
              label="Generate Ticket"
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

        <RunButton :loading="loading" :disabled="!cdnDir" @click="execute">
          Convert
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>
