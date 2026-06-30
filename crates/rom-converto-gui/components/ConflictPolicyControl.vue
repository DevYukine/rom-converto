<script setup lang="ts">
withDefaults(
  defineProps<{
    modelValue: string;
    label?: string;
    disabledOptions?: string[];
    disabledReason?: string;
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
  { label: "Overwrite if invalid", value: "overwrite-invalid" },
];
</script>

<template>
  <SegmentedControl
    :model-value="modelValue"
    :options="options"
    :label="label"
    :disabled-options="disabledOptions"
    :disabled-reason="disabledReason"
    @update:model-value="$emit('update:modelValue', $event)"
  />
</template>
