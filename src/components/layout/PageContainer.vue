<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  class?: string
  width?: 'default' | 'wide' | 'full'
}

const props = withDefaults(defineProps<Props>(), {
  width: 'default'
})
</script>

<template>
  <div
    :class="
      cn(
        'page-container min-w-0 px-4 py-6 pb-24 sm:px-6 lg:px-8 lg:pb-8',
        props.width === 'default' ? 'mx-auto max-w-[1440px]' : '',
        props.width === 'wide' ? 'mx-auto max-w-[1680px]' : '',
        props.class
      )
    "
  >
    <slot />
  </div>
</template>

<style scoped>
/*
 * 入场：页面内的直接子块（页头、提示、各区块）依次 fade + 上移 10px，错开 60ms。
 * 用 :deep(> *) 是因为插槽内容属于**父组件**的作用域，PageContainer 自己的
 * scope id 不会落在这些元素上。错开让"页面在组装起来"而不是"啪地出现"。
 */
.page-container :deep(> *) {
  animation: glass-rise var(--duration-glass) var(--ease-glass) both;
}
.page-container :deep(> *:nth-child(2)) {
  animation-delay: 60ms;
}
.page-container :deep(> *:nth-child(3)) {
  animation-delay: 120ms;
}
.page-container :deep(> *:nth-child(4)) {
  animation-delay: 180ms;
}
.page-container :deep(> *:nth-child(n + 5)) {
  animation-delay: 240ms;
}
@media (prefers-reduced-motion: reduce) {
  .page-container :deep(> *) {
    animation: none;
  }
}
</style>
