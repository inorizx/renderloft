<script setup lang="ts">
import { nextTick, ref, watch, onBeforeUnmount, onMounted } from 'vue'
import { cn } from '@/lib/utils'
import { useLocale } from '@/shared/composables/useLocale'

interface Props {
  open?: boolean
  title?: string
  class?: string
  style?: Record<string, string | number>
  bodyClass?: string
  mobileFullscreen?: boolean
  frameless?: boolean
  animated?: boolean
}

const props = withDefaults(defineProps<Props>(), { animated: true })
const emit = defineEmits<{ 'update:open': [value: boolean] }>()

const { t } = useLocale()

const panelRef = ref<HTMLElement | null>(null)
const titleId = `dialog-title-${Math.random().toString(36).slice(2, 8)}`
let previouslyFocused: HTMLElement | null = null

const close = () => emit('update:open', false)

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])'
].join(',')

const focusableElements = (): HTMLElement[] => {
  const panel = panelRef.value
  if (!panel) return []
  return Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) => el.offsetParent !== null || el === document.activeElement
  )
}

const focusFirst = () => {
  const elements = focusableElements()
  ;(elements[0] ?? panelRef.value)?.focus()
}

watch(
  () => props.open,
  async (open) => {
    if (open) {
      previouslyFocused = document.activeElement as HTMLElement | null
      await nextTick()
      focusFirst()
    } else {
      previouslyFocused?.focus?.()
      previouslyFocused = null
    }
  }
)

const handleKeydown = (event: KeyboardEvent) => {
  if (!props.open) return
  if (event.key === 'Escape') {
    close()
    return
  }
  // Tab 焦点圈闭：保持在对话框内循环，避免键盘用户跳出 modal 契约。
  if (event.key === 'Tab') {
    const panel = panelRef.value
    if (!panel) return
    const traversable = focusableElements()
    if (traversable.length === 0) {
      event.preventDefault()
      return
    }
    const first = traversable[0]
    const last = traversable[traversable.length - 1]
    const active = document.activeElement
    if (event.shiftKey && (active === first || !panel.contains(active))) {
      event.preventDefault()
      last.focus()
    } else if (!event.shiftKey && (active === last || !panel.contains(active))) {
      event.preventDefault()
      first.focus()
    }
  }
}

onMounted(() => document.addEventListener('keydown', handleKeydown))
onBeforeUnmount(() => {
  document.removeEventListener('keydown', handleKeydown)
  // 关闭时归还焦点到打开前的元素
  previouslyFocused?.focus?.()
  previouslyFocused = null
})
</script>

<template>
  <Teleport to="body">
    <Transition name="dialog">
      <div
        v-if="open"
        class="fixed inset-0 z-dialog flex items-center justify-center p-4"
        :class="{
          'dialog-mobile-fullscreen': props.mobileFullscreen,
          'dialog-no-animation': !props.animated
        }"
        @click.self="close"
      >
        <!-- 遮罩走 `bg-scrim`（不是 `bg-foreground/25`）：暗色主题下 foreground 是浅色，
             用它当遮罩会把背面的内容刷白。语义见 style.css 的 `--scrim`。 -->
        <div
          class="absolute inset-0 bg-scrim backdrop-blur-md"
          :class="props.frameless && 'dialog-lightbox-backdrop'"
          aria-hidden="true"
          @click="close"
        />
        <section
          ref="panelRef"
          role="dialog"
          aria-modal="true"
          tabindex="-1"
          :aria-label="title || t('common.dialog')"
          :aria-labelledby="title ? titleId : undefined"
          :style="props.style"
          :class="
            cn(
              'glass-panel relative z-10 w-full max-w-lg rounded-xl text-card-foreground outline-none',
              props.frameless && 'dialog-frameless',
              props.class
            )
          "
        >
          <div
            v-if="!props.frameless"
            class="flex min-h-14 items-center justify-between border-b border-border px-5"
          >
            <h3 :id="titleId" class="text-base font-semibold">{{ title }}</h3>
            <button
              type="button"
              :aria-label="t('common.closeDialog')"
              class="inline-flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground transition-colors duration-150 hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card disabled:pointer-events-none disabled:opacity-50"
              @click="close"
            >
              <svg
                class="h-4 w-4"
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
                aria-hidden="true"
              >
                <path
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  stroke-width="2"
                  d="M6 18 18 6M6 6l12 12"
                />
              </svg>
            </button>
          </div>
          <div :class="cn('p-5', props.bodyClass)"><slot /></div>
          <div
            v-if="$slots.footer"
            class="flex items-center justify-end gap-2 border-t border-border px-5 py-3"
          >
            <slot name="footer" />
          </div>
        </section>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.dialog-enter-active,
.dialog-leave-active {
  transition: opacity var(--duration-normal) var(--ease-standard);
}

.dialog-enter-active section,
.dialog-leave-active section {
  transition:
    opacity var(--duration-normal) var(--ease-standard),
    transform var(--duration-normal) var(--ease-standard);
}

.dialog-enter-from,
.dialog-leave-to,
.dialog-enter-from section,
.dialog-leave-to section {
  opacity: 0;
}
.dialog-frameless {
  border: 0;
  background: transparent;
  box-shadow: none;
}
.dialog-lightbox-backdrop {
  background: hsl(0 0% 0% / 0.9);
  backdrop-filter: none;
}

.dialog-enter-from section,
.dialog-leave-to section {
  transform: translateY(4px) scale(0.98);
}
.dialog-no-animation,
.dialog-no-animation section {
  transition: none !important;
}
.dialog-no-animation.dialog-enter-from,
.dialog-no-animation.dialog-leave-to,
.dialog-no-animation.dialog-enter-from section,
.dialog-no-animation.dialog-leave-to section {
  opacity: 1;
  transform: none;
}

@media (max-width: 767px) {
  .dialog-mobile-fullscreen {
    padding: 0;
  }
  .dialog-mobile-fullscreen section {
    width: 100%;
    max-width: none;
    height: 100dvh;
    border: 0;
    border-radius: 0;
  }
}
</style>
