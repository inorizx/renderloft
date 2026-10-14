<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  modelValue?: string
  class?: string
}

const props = defineProps<Props>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const tabs = defineModel<{ id: string; label: string }[]>('tabs', { default: () => [] })

const selectTab = (id: string) => emit('update:modelValue', id)
</script>

<template>
  <div :class="props.class">
    <div class="flex border-b border-border" role="tablist">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        type="button"
        role="tab"
        :aria-selected="modelValue === tab.id"
        :class="
          cn(
            'relative px-3 py-2.5 text-sm font-medium transition-colors duration-150 focus-visible:z-10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset disabled:pointer-events-none disabled:opacity-50',
            modelValue === tab.id
              ? 'text-primary'
              : 'text-muted-foreground hover:bg-accent/70 hover:text-foreground'
          )
        "
        @click="selectTab(tab.id)"
      >
        {{ tab.label }}
        <span
          v-if="modelValue === tab.id"
          class="absolute inset-x-3 bottom-0 h-0.5 rounded-full bg-primary"
        />
      </button>
    </div>
    <div class="p-4"><slot /></div>
  </div>
</template>
