<script setup lang="ts">
import { Button, Dialog } from '@/components/ui'
import { useLocale } from '@/shared/composables/useLocale'

interface Props {
  open?: boolean
  title: string
  description?: string
  confirmLabel?: string
  cancelLabel?: string
  destructive?: boolean
  isConfirming?: boolean
}

const { t } = useLocale()
// 不给 confirmLabel / cancelLabel 设静态默认值：默认文案改为在模板里按当前语言求值（否则切语言不刷新）。
defineProps<Props>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  confirm: []
}>()
</script>

<template>
  <Dialog :open="open" :title="title" @update:open="emit('update:open', $event)">
    <p v-if="description" class="text-sm leading-6 text-muted-foreground">{{ description }}</p>
    <slot />
    <template #footer>
      <Button variant="outline" type="button" @click="emit('update:open', false)">{{
        cancelLabel ?? t('common.cancel')
      }}</Button>
      <Button
        :class="
          destructive
            ? 'bg-destructive text-destructive-foreground hover:bg-destructive/90'
            : undefined
        "
        type="button"
        :disabled="isConfirming"
        @click="emit('confirm')"
      >
        {{ isConfirming ? t('common.processing') : (confirmLabel ?? t('common.confirm')) }}
      </Button>
    </template>
  </Dialog>
</template>
