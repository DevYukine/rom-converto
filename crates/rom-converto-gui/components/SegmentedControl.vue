<script setup lang="ts">
defineProps<{
  modelValue: string;
  options: { label: string; value: string }[];
  label?: string;
  disabled?: boolean;
}>();

defineEmits<{
  "update:modelValue": [value: string];
}>();
</script>

<template>
  <div class="space-y-1.5">
    <label v-if="label" class="block text-sm font-medium text-zinc-300">{{ label }}</label>
    <div role="group" class="inline-flex gap-0.5 rounded-lg border border-zinc-700 bg-zinc-800/30 p-0.5">
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        :disabled="disabled"
        :aria-pressed="option.value === modelValue"
        class="rounded-md px-4 py-1.5 text-sm font-medium transition"
        :class="[
          option.value === modelValue
            ? 'bg-sky-500 text-white shadow'
            : 'text-zinc-400 hover:text-zinc-200',
          disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
        ]"
        @click="!disabled && option.value !== modelValue && $emit('update:modelValue', option.value)"
      >
        {{ option.label }}
      </button>
    </div>
  </div>
</template>
