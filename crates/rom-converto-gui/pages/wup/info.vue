<script setup lang="ts">
import { ref } from "vue";
import { storeToRefs } from "pinia";
import { useWupInfoStore } from "~/stores/wup-info";

const store = useWupInfoStore();
const { input, keys, info, rawJson, error, loading } = storeToRefs(store);

const WUA_FILTERS = [
  { name: "WUA archive or disc image", extensions: ["wua", "wud", "wux", "zip", "7z", "rar", "tar", "tgz", "gz"] },
];

type SourceKind = "directory" | "wua";
const sourceKind = ref<SourceKind>("directory");

const SOURCE_OPTIONS = [
  { label: "Title directory", value: "directory" },
  { label: "Archive / disc image", value: "wua" },
];

function selectSource(kind: SourceKind) {
  if (sourceKind.value !== kind) {
    sourceKind.value = kind;
    input.value = "";
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Wii U info"
      description="Read a NUS or loadiine title directory, a .wua archive, or a .wud / .wux disc image: TMD, multilingual names from meta.xml, region, save sizes, accessories, and age ratings."
      :loading="loading"
      :has-result="!!info"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :error="error" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <div>
          <SegmentedControl
            :model-value="sourceKind"
            label="Source"
            :options="SOURCE_OPTIONS"
            @update:model-value="(v: string) => selectSource(v as SourceKind)"
          />
          <p class="mt-1.5 text-xs text-zinc-500">
            <template v-if="sourceKind === 'directory'">
              Pick a decrypted NUS or loadiine title folder.
            </template>
            <template v-else>
              Pick a Cemu .wua archive or a .wud / .wux disc image. Archives bundling multiple
              titles show the first one. Encrypted discs need the disc master key below.
            </template>
          </p>
        </div>

        <FileDropZone
          v-if="sourceKind === 'directory'"
          v-model="input"
          label="Title directory"
          :primary="true"
          directory
        />
        <FileDropZone
          v-else
          v-model="input"
          label="Archive or disc image"
          :primary="true"
          :filters="WUA_FILTERS"
        />

        <FileDropZone
          v-model="keys"
          label="Disc master key (for .wud / .wux)"
          :filters="[{ name: 'Disc key', extensions: ['key', 'bin', 'txt'] }]"
        />

        <RunButton
          :loading="loading"
          :disabled="!input"
          @click="store.execute"
        >
          Read info
        </RunButton>

        <RomInfoCard v-if="info" :info="info" />
      </div>
    </OperationCard>

    <details v-if="rawJson" class="mt-4">
      <summary class="cursor-pointer text-sm text-zinc-500">Raw JSON payload</summary>
      <pre class="mt-2 overflow-auto rounded bg-black/40 p-3 text-xs">{{ rawJson }}</pre>
    </details>
  </div>
</template>
