<script setup lang="ts">
defineProps<{
  recursive: boolean;
  maxDepth: number | null;
}>();

defineEmits<{
  "update:recursive": [value: boolean];
  "update:maxDepth": [value: number | null];
}>();

function onDepthInput(event: Event) {
  const raw = (event.target as HTMLInputElement).value;
  return raw === "" ? null : Number(raw);
}
</script>

<template>
  <div class="space-y-3">
    <FlagToggle
      :model-value="recursive"
      label="Recursive"
      description="Scan the dropped folder and process every file inside it"
      @update:model-value="$emit('update:recursive', $event)"
    />
    <div v-if="recursive" class="space-y-1.5">
      <label class="block text-sm font-medium text-zinc-300">Max depth (optional)</label>
      <input
        type="number"
        min="1"
        :value="maxDepth ?? ''"
        placeholder="Unlimited"
        class="w-32 rounded-lg border border-zinc-700 bg-zinc-800/30 px-3 py-1.5 text-sm text-zinc-200 focus:border-sky-500 focus:outline-none"
        @input="$emit('update:maxDepth', onDepthInput($event))"
      >
    </div>
  </div>
</template>
