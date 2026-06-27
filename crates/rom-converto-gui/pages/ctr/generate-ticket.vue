<script setup lang="ts">
import { save } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { useCtrGenerateTicketStore } from "~/stores/ctr-generate-ticket";

const store = useCtrGenerateTicketStore();
const { cdnDir, output, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const commandLine = ref("");

async function chooseOutput() {
  const picked = await save({
    defaultPath: "ticket.tik",
    filters: [{ name: "Ticket", extensions: ["tik"] }],
  });
  if (picked) output.value = resolve(picked);
}

watch(outputDir, () => {
  if (output.value) output.value = resolve(basename(output.value));
});

async function execute() {
  const args = { cdnDir: cdnDir.value, output: output.value };
  commandLine.value = buildCliCommand("cmd_generate_ticket", args);
  await run("cmd_generate_ticket", args);
}
</script>

<template>
  <div>
    <PageHeader
      title="Generate ticket"
      description="Synthesize a .tik ticket from the title key and metadata in a CDN content directory."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="cdnDir"
          label="CDN directory"
          :directory="true"
          :primary="true"
        />

        <div class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">Output ticket file</label>
          <div class="flex items-center justify-between rounded-lg border border-zinc-700 bg-zinc-800/30 px-4 py-3">
            <span class="truncate text-sm" :class="output ? 'text-zinc-200' : 'text-zinc-500'" :title="output">
              {{ output || "No output chosen" }}
            </span>
            <button
              type="button"
              class="ml-3 shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
              @click="chooseOutput"
            >
              Choose output
            </button>
          </div>
        </div>

        <OutputDirField v-model="outputDir" />

        <RunButton :loading="loading" :disabled="!cdnDir || !output" @click="execute">
          Generate
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
