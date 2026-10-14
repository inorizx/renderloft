<script setup lang="ts">
import { type ClassValue, cn } from '@/lib/utils'

interface Props {
  /**
   * 与 `cn` 同一个类型：调用点可以照 Vue 的习惯传条件数组
   * （`:class="['a', isX && 'b']"`）。写成 `string` 会让这种写法在 vue-tsc 阶段报错。
   */
  class?: ClassValue
}

const props = defineProps<Props>()
</script>

<template>
  <div
    :class="
      cn(
        'glass-panel rounded-lg text-card-foreground transition-[border-color,box-shadow] duration-150',
        props.class
      )
    "
  >
    <slot />
  </div>
</template>
