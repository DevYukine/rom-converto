<script setup lang="ts">
const cdnDir = ref("");
const output = ref("");
const decrypt = ref(true);
const compress = ref(false);
const cleanup = ref(false);
const recursive = ref(false);
const ensureTicket = ref(true);

const { result, error, loading, run } = useOperation();
const progress = useProgress("cdn-to-cia");

watch(compress, (val) => {
  if (val) decrypt.value = true;
});

async function execute() {
  progress.reset();
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
  <div class="mx-auto max-w-xl space-y-6">
    <h2 class="text-xl font-semibold">CDN to CIA</h2>
    <p class="text-sm text-zinc-400">
      Convert Nintendo CDN content to CIA format.
    </p>

    <FileDropZone
      v-model="cdnDir"
      label="CDN Directory"
      :directory="true"
      :primary="true"
    />

    <FileDropZone
      v-model="output"
      label="Output (optional)"
      :filters="[{ name: 'CIA', extensions: ['cia'] }]"
    />

    <div class="space-y-3 rounded-lg border border-zinc-800 p-4">
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

    <ProgressBar
      :percent="progress.percent.value"
      :message="progress.message.value"
      :running="progress.running.value"
    />

    <RunButton :loading="loading" :disabled="!cdnDir" @click="execute">
      Convert
    </RunButton>

    <OutputLog :result="result" :error="error" />
  </div>
</template>
