<script setup lang="ts">
defineProps<{
  label: string;
  modelValue: boolean;
  description?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
}>();
</script>

<template>
  <label class="flex cursor-pointer items-center gap-3">
    <button
      type="button"
      role="switch"
      :aria-checked="modelValue"
      class="relative h-5 w-9 rounded-full transition"
      :class="modelValue ? 'bg-sky-500' : 'bg-zinc-600'"
      @click="emit('update:modelValue', !modelValue)"
    >
      <span
        class="absolute top-0.5 left-0.5 h-4 w-4 rounded-full bg-white transition-transform"
        :class="modelValue ? 'translate-x-4' : 'translate-x-0'"
      />
    </button>
    <div>
      <span class="text-sm font-medium text-zinc-200">{{ label }}</span>
      <p v-if="description" class="text-xs text-zinc-500">{{ description }}</p>
    </div>
  </label>
</template>
