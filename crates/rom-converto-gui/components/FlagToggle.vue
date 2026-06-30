<script setup lang="ts">
const props = defineProps<{
  label: string;
  modelValue: boolean;
  description?: string;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
}>();

const uid = useId();
const labelId = `${uid}-label`;
const descriptionId = `${uid}-description`;

function toggle() {
  if (!props.disabled) {
    emit("update:modelValue", !props.modelValue);
  }
}
</script>

<template>
  <div
    class="flex items-center justify-between gap-4 py-1"
    :class="disabled ? 'cursor-not-allowed' : 'cursor-pointer'"
    @click="toggle"
  >
    <div class="min-w-0">
      <span :id="labelId" class="text-sm font-medium text-zinc-200">{{ label }}</span>
      <p v-if="description" :id="descriptionId" class="text-xs text-zinc-500">{{ description }}</p>
    </div>
    <button
      type="button"
      role="switch"
      :aria-checked="modelValue"
      :aria-labelledby="labelId"
      :aria-describedby="description ? descriptionId : undefined"
      :disabled="disabled"
      class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors"
      :class="[
        modelValue ? 'bg-sky-500' : 'bg-zinc-600',
        disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer',
      ]"
    >
      <span
        class="inline-block h-3.5 w-3.5 rounded-full bg-white shadow transition-transform"
        :class="modelValue ? 'translate-x-4.5' : 'translate-x-0.5'"
      />
    </button>
  </div>
</template>
