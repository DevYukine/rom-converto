<script setup lang="ts">
import { storeToRefs } from "pinia";
import { usePlaylistStore } from "~/stores/playlist";

const store = usePlaylistStore();
const { scanDir, outputDir, mode, extensions, maxDepth, onConflict, result, error, loading } =
  storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("playlist");
const commandLine = ref("");

const MODE_OPTIONS = [
  { label: "Multiple discs only", value: "multiple" },
  { label: "Always", value: "always" },
];

function onDepthInput(event: Event) {
  const raw = (event.target as HTMLInputElement).value;
  maxDepth.value = raw === "" ? null : Number(raw);
}

function playlistArgs() {
  return {
    scanDir: scanDir.value,
    outputDir: outputDir.value || null,
    mode: mode.value,
    extensions: extensions.value,
    maxDepth: maxDepth.value,
    onConflict: onConflict.value,
  };
}

async function execute() {
  progress.reset();
  const args = playlistArgs();
  commandLine.value = buildCliCommand("cmd_playlist", args);
  await run("cmd_playlist", args);
}
</script>

<template>
  <div>
    <PageHeader
      title="Playlist"
      description="Scan a folder for multi-disc sets and write .m3u playlists so emulators can swap discs. By default only sets with more than one disc get a playlist."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <FileDropZone
          v-model="scanDir"
          label="Scan folder"
          :directory="true"
          :primary="true"
        />

        <OutputDirField v-model="outputDir" />

        <div class="space-y-4 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <SegmentedControl v-model="mode" :options="MODE_OPTIONS" label="When to write a playlist" />

          <div class="space-y-1.5">
            <label class="block text-sm font-medium text-zinc-300">Extensions</label>
            <input
              v-model="extensions"
              type="text"
              placeholder="cue,chd,iso,cso,zso"
              class="w-full rounded-lg border border-zinc-700 bg-zinc-800/30 px-3 py-1.5 text-sm text-zinc-200 focus:border-sky-500 focus:outline-none"
            >
          </div>

          <div class="space-y-1.5">
            <label class="block text-sm font-medium text-zinc-300">Max depth (optional)</label>
            <input
              type="number"
              min="1"
              :value="maxDepth ?? ''"
              placeholder="Unlimited"
              class="w-32 rounded-lg border border-zinc-700 bg-zinc-800/30 px-3 py-1.5 text-sm text-zinc-200 focus:border-sky-500 focus:outline-none"
              @input="onDepthInput"
            >
          </div>

          <ConflictPolicyControl v-model="onConflict" />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton :loading="loading" :disabled="!scanDir" @click="execute">
          Write playlists
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>
