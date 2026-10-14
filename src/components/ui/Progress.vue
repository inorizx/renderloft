<script setup lang="ts">
import { computed } from 'vue'

interface Props {
  value?: number
  max?: number
  label?: string
  showLabel?: boolean
  class?: string
}

const props = withDefaults(defineProps<Props>(), { max: 100, showLabel: true })
const percentage = computed(() =>
  Math.min(100, Math.max(0, ((props.value ?? 0) / props.max) * 100))
)
</script>

<template>
  <div :class="props.class">
    <div v-if="label || showLabel" class="mb-2 flex items-center justify-between">
      <span v-if="label" class="text-sm font-medium text-foreground">{{ label }}</span>
      <span v-if="showLabel" class="text-sm tabular-nums text-muted-foreground"
        >{{ Math.round(percentage) }}%</span
      >
    </div>
    <div
      class="h-1.5 w-full overflow-hidden rounded-full bg-muted"
      role="progressbar"
      :aria-valuenow="value"
      :aria-valuemin="0"
      :aria-valuemax="max"
    >
      <div
        class="h-full rounded-full bg-primary transition-[width] duration-200 ease-standard"
        :style="{ width: `${percentage}%` }"
      />
    </div>
  </div>
</template>
