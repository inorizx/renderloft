<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  variant?: 'default' | 'secondary' | 'outline' | 'ghost' | 'link'
  size?: 'default' | 'sm' | 'lg' | 'icon'
  class?: string
}

const props = withDefaults(defineProps<Props>(), {
  variant: 'default',
  size: 'default'
})
</script>

<template>
  <button
    :class="
      cn(
        'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md font-medium transition-[background-color,border-color,color,box-shadow,transform] duration-150 ease-standard focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 active:translate-y-px',
        props.variant === 'default' && 'bg-primary text-primary-foreground hover:bg-primary/90',
        props.variant === 'secondary' &&
          'bg-secondary text-secondary-foreground hover:bg-secondary/80',
        props.variant === 'outline' &&
          'border border-input bg-secondary hover:bg-accent hover:text-accent-foreground',
        props.variant === 'ghost' && 'hover:bg-accent hover:text-accent-foreground',
        props.variant === 'link' && 'h-auto px-0 text-primary underline-offset-4 hover:underline',
        props.size === 'default' && 'h-9 px-3.5 text-sm',
        props.size === 'sm' && 'h-8 px-3 text-xs',
        props.size === 'lg' && 'h-10 px-5 text-sm',
        props.size === 'icon' && 'h-9 w-9',
        props.class
      )
    "
  >
    <slot />
  </button>
</template>
