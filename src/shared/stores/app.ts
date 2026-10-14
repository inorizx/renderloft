import { defineStore } from 'pinia'
import { ref } from 'vue'

/** 通知上可以挂一个动作（例如下载完成后"在文件夹中显示"）。 */
export interface NotificationAction {
  label: string
  run: () => void
}

export const useAppStore = defineStore('app', () => {
  const isLoading = ref(false)
  const notification = ref<{
    type: 'success' | 'error' | 'info' | 'warning'
    message: string
    action?: NotificationAction
  } | null>(null)
  let notificationTimer: ReturnType<typeof setTimeout> | undefined

  const setLoading = (loading: boolean) => {
    isLoading.value = loading
  }

  /**
   * 弹一条通知。`action` 是可选的——**带动作的通知不自动消失**。
   *
   * 理由：动作是给人点的（"在文件夹中显示"），3 秒就飘走等于没有；
   * 浏览器与主流下载器的下载提示本来也是常驻的。纯提示仍然 3 秒自动收。
   */
  const showNotification = (
    type: 'success' | 'error' | 'info' | 'warning',
    message: string,
    action?: NotificationAction
  ) => {
    notification.value = { type, message, action }
    if (notificationTimer) clearTimeout(notificationTimer)
    if (action) return
    notificationTimer = setTimeout(() => {
      notification.value = null
    }, 3000)
  }
  const dismissNotification = () => {
    if (notificationTimer) clearTimeout(notificationTimer)
    notification.value = null
  }

  return {
    isLoading,
    notification,
    setLoading,
    showNotification,
    dismissNotification
  }
})
