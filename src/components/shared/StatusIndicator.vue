<script setup lang="ts">
interface Props {
  status?: 'default' | 'success' | 'warning' | 'error' | 'info'
  label?: string
}

withDefaults(defineProps<Props>(), {
  status: 'default'
})
</script>

<template>
  <span class="inline-flex items-center gap-2 text-xs text-muted-foreground">
    <!-- 圆点用 mark 色：它和右侧文字是冗余指示，可以鲜亮（见 style.css 的状态色注释） -->
    <span
      :class="[
        'size-2 rounded-full',
        status === 'success' ? 'bg-success-mark' : '',
        status === 'warning' ? 'bg-warning-mark' : '',
        status === 'error' ? 'bg-destructive-mark' : '',
        status === 'info' ? 'bg-info-mark' : '',
        status === 'default' ? 'bg-muted-foreground' : ''
      ]"
    />
    <slot>{{ label }}</slot>
  </span>
</template>
