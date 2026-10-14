import { ref } from 'vue'
import { defineStore } from 'pinia'
import { TASKCENTER_OVERLAY_MAX_WIDTH } from '@/shared/config/layout'

export const useTaskCenterStore = defineStore('task-center', () => {
  /**
   * 默认展开条件：视口足够宽，任务中心是**常驻侧栏**（主区会让出 320px）。
   *
   * 窄于该阈值时它是浮层 —— 浮层默认收起，否则一进页面就盖住内容。
   * 阈值见 `shared/config/layout.ts`（与 AppShell 的让位媒体查询同源）。
   */
  const isOpen = ref(
    typeof window === 'undefined' || window.innerWidth >= TASKCENTER_OVERLAY_MAX_WIDTH
  )

  return { isOpen }
})
