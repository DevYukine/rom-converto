<script setup lang="ts">
import { ref } from "vue";
import { storeToRefs } from "pinia";
import { useWupInfoStore } from "~/stores/wup-info";

const store = useWupInfoStore();
const { input, info, rawJson, error, loading } = storeToRefs(store);

const WUA_FILTERS = [{ name: "WUA archive", extensions: ["wua"] }];

type SourceKind = "directory" | "wua";
const sourceKind = ref<SourceKind>("directory");

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
      description="Read a NUS or loadiine title directory, or a .wua archive: TMD, multilingual names from meta.xml, region, save sizes, accessories, and age ratings. WUD and WUX disc images are not yet supported."
      :loading="loading"
      :has-result="!!info"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <div>
          <div class="mb-2 block text-sm font-medium text-zinc-300">Source</div>
          <div class="inline-flex rounded-lg border border-zinc-700 bg-zinc-800/50 p-0.5">
            <button
              type="button"
              :class="[
                'rounded-md px-3 py-1.5 text-xs font-medium transition',
                sourceKind === 'directory'
                  ? 'bg-zinc-700 text-zinc-100'
                  : 'text-zinc-400 hover:text-zinc-200',
              ]"
              @click="selectSource('directory')"
            >
              Title directory
            </button>
            <button
              type="button"
              :class="[
                'rounded-md px-3 py-1.5 text-xs font-medium transition',
                sourceKind === 'wua'
                  ? 'bg-zinc-700 text-zinc-100'
                  : 'text-zinc-400 hover:text-zinc-200',
              ]"
              @click="selectSource('wua')"
            >
              .wua archive
            </button>
          </div>
          <p class="mt-1.5 text-xs text-zinc-500">
            <template v-if="sourceKind === 'directory'">
              Pick a decrypted NUS or loadiine title folder.
            </template>
            <template v-else>
              Pick a Cemu .wua archive. Archives bundling multiple titles show the first one.
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
          label="WUA archive"
          :primary="true"
          :filters="WUA_FILTERS"
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

    <div class="mt-4">
      <OutputLog :error="error" />
    </div>

    <details v-if="rawJson" class="mt-4">
      <summary class="cursor-pointer text-sm text-zinc-500">Raw JSON payload</summary>
      <pre class="mt-2 overflow-auto rounded bg-black/40 p-3 text-xs">{{ rawJson }}</pre>
    </details>
  </div>
</template>
