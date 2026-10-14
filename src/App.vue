<template>
  <AppShell />
  <div
    v-if="appStore.notification"
    class="app-notification"
    :class="`app-notification-${appStore.notification.type}`"
    :role="appStore.notification.type === 'error' ? 'alert' : 'status'"
  >
    <CircleCheck v-if="appStore.notification.type === 'success'" class="size-4 shrink-0" />
    <CircleAlert
      v-else-if="appStore.notification.type === 'error' || appStore.notification.type === 'warning'"
      class="size-4 shrink-0"
    />
    <Info v-else class="size-4 shrink-0" />
    <span>{{ appStore.notification.message }}</span>
    <!-- 带动作的通知（如"在文件夹中显示"）不自动消失，见 app store 里的说明。 -->
    <button
      v-if="appStore.notification.action"
      type="button"
      class="app-notification-action"
      @click="runNotificationAction"
    >
      {{ appStore.notification.action.label }}
    </button>
    <IconButton
      label="关闭提示"
      size="sm"
      class="app-notification-close"
      @click="appStore.dismissNotification()"
    >
      <X class="size-4" />
    </IconButton>
  </div>
</template>

<script setup lang="ts">
import { CircleAlert, CircleCheck, Info, X } from '@lucide/vue'
import AppShell from '@/components/AppShell.vue'
import { IconButton } from '@/components/ui'
import { useAppStore } from '@/shared/stores/app'

const appStore = useAppStore()

/** 跑通知上的动作（例如"在文件夹中显示"），跑完就把通知收掉——动作是一次性的。 */
const runNotificationAction = () => {
  const action = appStore.notification?.action
  appStore.dismissNotification()
  action?.run()
}
</script>

<style scoped>
.app-notification {
  position: fixed;
  right: 1rem;
  bottom: 1rem;
  z-index: var(--z-toast);
  display: flex;
  max-width: min(30rem, calc(100vw - 2rem));
  align-items: center;
  gap: 0.625rem;
  border: 1px solid var(--border);
  border-left-width: 3px;
  border-radius: 0.375rem;
  background: var(--card);
  padding: 0.625rem 0.625rem 0.625rem 0.75rem;
  box-shadow: 0 12px 30px color-mix(in srgb, black 16%, transparent);
  font-size: 0.875rem;
  color: var(--foreground);
  /*
   * ⚠️ 入场用 **CSS 动画**，不要用 `<Transition>` 的 enter-from/enter-active。
   *
   * `<Transition>` 是"先给元素挂 opacity:0，等下一帧再摘掉"——摘掉的那一步走
   * `requestAnimationFrame`。而 WebView2 在窗口被遮挡/最小化时**会停掉帧**
   * （Chromium 的 native window occlusion tracking），那一帧永远不来 ⇒ 提示条
   * 停在 opacity:0：DOM 里有、位置也对、z-index 也对，**就是看不见**，3 秒后
   * 连状态一起被清掉。于是"点了测试没反应"。实测复现：一个被节流的浏览器里，
   * 点击后 500ms / 1.5s 两次采样，`.app-notification` 的 opacity 都是 0。
   *
   * 关键帧动画没有这个失败模式：帧不跑时元素就停在**自然状态**（opacity:1，可见），
   * 帧跑起来才补上那 150ms 的入场。
   */
  animation: notification-in 150ms ease-out;
}
@keyframes notification-in {
  from {
    opacity: 0;
    transform: translateY(0.5rem);
  }
}
.app-notification-success {
  border-left-color: var(--success);
}
.app-notification-error {
  border-left-color: var(--destructive);
}
.app-notification-warning {
  border-left-color: var(--warning);
}
.app-notification-info {
  border-left-color: var(--primary);
}
.app-notification-close {
  margin-left: 0.25rem;
}
/* 动作按钮：主流下载提示里的"在文件夹中显示"就是这种位置与分量（次要、可点、不抢眼）。 */
.app-notification-action {
  margin-left: 0.5rem;
  flex: none;
  border-radius: var(--radius);
  border: 1px solid var(--border);
  padding: 0.125rem 0.5rem;
  font-size: 0.75rem;
  font-weight: 500;
  color: var(--primary);
  white-space: nowrap;
  transition: background-color 120ms ease;
}
.app-notification-action:hover {
  background: var(--accent);
}
@media (max-width: 767px) {
  .app-notification {
    right: 0.75rem;
    bottom: 0.75rem;
  }
}
</style>
