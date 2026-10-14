<script setup lang="ts">
import { computed } from 'vue'
import { cn } from '@/lib/utils'

type Tone = 'neutral' | 'success' | 'warning' | 'error' | 'info'

interface Props {
  status?: string
  label?: string
  class?: string
}

const props = withDefaults(defineProps<Props>(), { status: 'neutral' })

/** 业务状态字符串 → 徽标色调。未识别的状态回退为中性。 */
const statusTone = (status: string): Tone => {
  switch (status) {
    case 'COMPLETED':
    case 'SUCCEEDED':
    case 'ACCEPTED':
    case 'success':
      return 'success'
    case 'PARTIAL':
    case 'WORKING':
    case 'SUBMITTED':
    case 'warning':
      return 'warning'
    case 'FAILED':
    case 'CANCELLED':
    case 'REJECTED':
    case 'error':
      return 'error'
    case 'INPUT_REQUIRED':
    case 'info':
      return 'info'
    default:
      return 'neutral'
  }
}

const tone = computed(() => statusTone(props.status ?? 'neutral'))

/**
 * 文字与底：淡底取自 **mark**（鲜亮色）而不是文字色 —— 12% 的墨绿是脏米色，
 * 12% 的鲜绿才是干净的浅绿，而且文字在新底上的对比度还更高（见 style.css 的状态色注释）。
 */
const toneClass = computed(
  () =>
    ({
      neutral: 'bg-muted text-muted-foreground',
      success: 'bg-success-mark/12 text-success',
      warning: 'bg-warning-mark/12 text-warning',
      error: 'bg-destructive-mark/12 text-destructive',
      info: 'bg-info-mark/12 text-info'
    })[tone.value]
)

/**
 * 圆点用 mark 色，且**不再跟 currentColor**：它和文字是冗余指示（文字永远在），
 * 不承担"只靠颜色区分状态"的语义，所以可以鲜亮；跟着文字色走才是它发土的原因。
 */
const dotClass = computed(
  () =>
    ({
      neutral: 'bg-muted-foreground',
      success: 'bg-success-mark',
      warning: 'bg-warning-mark',
      error: 'bg-destructive-mark',
      info: 'bg-info-mark'
    })[tone.value]
)
</script>

<template>
  <span
    :class="
      cn(
        'inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-xs font-medium leading-tight',
        toneClass,
        props.class
      )
    "
  >
    <span :class="cn('h-1.5 w-1.5 rounded-full', dotClass)" aria-hidden="true" />
    <slot>{{ props.label }}</slot>
  </span>
</template>
