<script setup lang="ts">
withDefaults(
  defineProps<{
    modelValue: string;
    label?: string;
  }>(),
  { label: "On conflict" },
);

defineEmits<{
  "update:modelValue": [value: string];
}>();

const options = [
  { label: "Overwrite", value: "overwrite" },
  { label: "Skip", value: "skip" },
  { label: "Rename", value: "rename" },
  { label: "Error", value: "error" },
];
</script>

<template>
  <SegmentedControl
    :model-value="modelValue"
    :options="options"
    :label="label"
    @update:model-value="$emit('update:modelValue', $event)"
  />
</template>
