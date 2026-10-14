import { onBeforeUnmount, ref } from 'vue'

export interface PollingOptions<T> {
  /** 拉取最新状态；返回 null 表示暂时失败（会重试） */
  fetch: () => Promise<T | null>
  /** 状态或消息有变化时回调（用于刷新列表/侧栏） */
  onUpdate: (value: T) => void
  /** 是否应停止轮询（终态等） */
  isTerminal?: (value: T) => boolean
  /** 轮询间隔毫秒，默认 3000 */
  intervalMs?: number
  /** 最大尝试次数，默认 120；达到后停止但保留 stop 可手动继续 */
  maxAttempts?: number
  /** 达到尝试上限时通知调用方显示恢复操作。 */
  onExhausted?: () => void
}

/**
 * 带卸载清理、网络错误重试、终态自动停止的轮询器。
 * 组件销毁或显式 stop() 后不再发起任何请求。
 */
export function usePolling<T>(options: PollingOptions<T>) {
  const active = ref(false)
  let stopped = false
  let timer: ReturnType<typeof setTimeout> | undefined
  let attempt = 0

  const stop = () => {
    stopped = true
    active.value = false
    if (timer) {
      clearTimeout(timer)
      timer = undefined
    }
  }

  const start = () => {
    if (active.value) return
    stopped = false
    active.value = true
    attempt = 0
    void tick()
  }

  const tick = async () => {
    if (stopped) return
    const maxAttempts = options.maxAttempts ?? 120
    const intervalMs = options.intervalMs ?? 3000
    if (attempt >= maxAttempts) {
      active.value = false
      options.onExhausted?.()
      return
    }
    attempt += 1
    const value = await options.fetch().catch(() => null)
    if (stopped) return
    if (value) {
      options.onUpdate(value)
      if (options.isTerminal?.(value)) {
        stop()
        return
      }
    }
    // 网络错误（fetch 返回 null）也继续轮询，直到达到次数上限。
    timer = setTimeout(() => void tick(), intervalMs)
  }

  onBeforeUnmount(stop)

  return { start, stop, active }
}
