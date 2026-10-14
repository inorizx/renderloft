<script setup lang="ts">
import { computed } from 'vue'

interface Props {
  modelValue?: number
  min?: number
  max?: number
  step?: number
  label?: string
  showValue?: boolean
  class?: string
}

const props = withDefaults(defineProps<Props>(), { min: 0, max: 100, step: 1, showValue: false })
const emit = defineEmits<{ 'update:modelValue': [value: number] }>()
const percentage = computed(() =>
  Math.min(
    100,
    Math.max(0, (((props.modelValue ?? props.min) - props.min) / (props.max - props.min)) * 100)
  )
)

const handleInput = (event: Event) =>
  emit('update:modelValue', Number((event.target as HTMLInputElement).value))
</script>

<template>
  <div :class="props.class">
    <div v-if="label" class="mb-2 flex items-center justify-between">
      <label class="text-sm font-medium text-foreground">{{ label }}</label>
      <span v-if="showValue" class="text-sm tabular-nums text-muted-foreground">{{
        modelValue
      }}</span>
    </div>
    <input
      type="range"
      :value="modelValue"
      :min="min"
      :max="max"
      :step="step"
      :style="{ backgroundSize: `${percentage}% 100%` }"
      class="ui-slider h-1.5 w-full cursor-pointer appearance-none rounded-full bg-muted bg-no-repeat focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50"
      @input="handleInput"
    />
  </div>
</template>

<style scoped>
/* 已填充轨道用半透明主题色（低于不透明度），圆钮用实心主题色：
   两段式配色让圆钮在填充/未填充上都清晰可见，且无白色，随日夜主题自动适配。 */
.ui-slider {
  background-image: linear-gradient(
    color-mix(in srgb, var(--primary) 48%, transparent),
    color-mix(in srgb, var(--primary) 48%, transparent)
  );
}
.ui-slider::-webkit-slider-thumb {
  width: 14px;
  height: 14px;
  appearance: none;
  border: none;
  border-radius: 9999px;
  background: var(--primary);
  box-shadow: var(--shadow-sm);
  cursor: pointer;
}
.ui-slider::-moz-range-thumb {
  width: 14px;
  height: 14px;
  border: none;
  border-radius: 9999px;
  background: var(--primary);
  box-shadow: var(--shadow-sm);
  cursor: pointer;
}
</style>
