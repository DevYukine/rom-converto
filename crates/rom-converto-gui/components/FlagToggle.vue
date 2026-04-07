<script setup lang="ts">
defineProps<{
  label: string;
  modelValue: boolean;
  description?: string;
}>();

defineEmits<{
  "update:modelValue": [value: boolean];
}>();
</script>

<template>
  <div class="flex items-center justify-between gap-4 py-1">
    <div class="min-w-0">
      <span class="text-sm font-medium text-zinc-200">{{ label }}</span>
      <p v-if="description" class="text-xs text-zinc-500">{{ description }}</p>
    </div>
    <button
      type="button"
      role="switch"
      :aria-checked="modelValue"
      class="relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full transition-colors"
      :class="modelValue ? 'bg-sky-500' : 'bg-zinc-600'"
      @click="$emit('update:modelValue', !modelValue)"
    >
      <span
        class="inline-block h-3.5 w-3.5 rounded-full bg-white shadow transition-transform"
        :class="modelValue ? 'translate-x-4.5' : 'translate-x-0.5'"
      />
    </button>
  </div>
</template>
