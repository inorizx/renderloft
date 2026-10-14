<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  variant?: 'default' | 'info' | 'success' | 'warning' | 'destructive'
  class?: string
}

const props = withDefaults(defineProps<Props>(), {
  variant: 'default'
})
</script>

<template>
  <section
    :class="
      cn(
        'glass-panel flex gap-3 rounded-lg p-4 text-sm leading-5',
        props.variant === 'default' ? 'text-muted-foreground' : '',
        props.variant === 'info' ? 'border-info/22 bg-info/[0.065] text-foreground' : '',
        props.variant === 'success' ? 'border-success/30 bg-success/10 text-foreground' : '',
        props.variant === 'warning' ? 'border-warning/30 bg-warning/10 text-foreground' : '',
        props.variant === 'destructive'
          ? 'notice-danger border-destructive/30 text-foreground'
          : '',
        props.class
      )
    "
  >
    <div v-if="$slots.icon" class="mt-0.5 shrink-0"><slot name="icon" /></div>
    <div class="min-w-0 flex-1"><slot /></div>
    <div v-if="$slots.actions" class="flex shrink-0 items-start"><slot name="actions" /></div>
  </section>
</template>

<style scoped>
/*
 * 危险态：底色仍取 --destructive 的淡色（由 bg-destructive/10 提供，叠在玻璃渐变之下），
 * 额外补一条左侧色条 —— 语义色需要"从形状上"也读得出来，不能只靠一层很淡的红。
 */
.notice-danger {
  background-color: color-mix(in srgb, var(--destructive) 12%, transparent);
  box-shadow:
    inset 2px 0 0 color-mix(in srgb, var(--destructive) 75%, transparent),
    var(--glass-panel-shadow);
}
</style>
