/**
 * 时间戳解析与相对时间文案。
 *
 * 抽成共享工具而不是各页再写一份：任务中心与 Agent 会话列表都要展示「多久以前」，
 * 两份实现必然漂移（本项目已有「长文本启发式收敛为单一权威、删除重复副本」的先例）。
 *
 * 这里刻意**不依赖 i18n 表**：文案由调用方传入，工具层保持纯粹可测。
 */

/**
 * 解析后端时间字段。
 *
 * 数值字符串按**毫秒时间戳**处理——后端确实返回 `"1791038418472"` 这种形态
 * （见 `/api/v1/deliveries` 的 `createdAt`），此时 `Date.parse` 会得到 NaN，
 * 所以必须先试数值。
 */
export function timestampOf(value: string): number {
  if (!value.trim()) return Number.NaN
  const numeric = Number(value)
  if (Number.isFinite(numeric)) return numeric
  return Date.parse(value)
}

export interface RelativeTimeLabels {
  /** 60 秒内统一显示的文案 */
  justNow: string
  /** 时间无法解析时的回退文案 */
  unknownTime: string
}

/**
 * 相对时间文案：<60s 归到「刚刚」，分钟级 / 小时级用 `Intl.RelativeTimeFormat`。
 *
 * <60s 一律归到「刚刚」的原因：秒级文案配 10s 时钟 tick 只会跳字（22→32→42），
 * 而列表里的时间无需精确到秒；分钟级以上的粒度才与 10s tick 匹配。
 *
 * 本函数**不订阅时钟**：需要自动刷新的调用方自己 tick（见 TaskCenter 的 `trackClock`）。
 */
export function formatRelativeTime(
  value: string,
  locale: string,
  labels: RelativeTimeLabels
): string {
  const timestamp = timestampOf(value)
  if (Number.isNaN(timestamp)) return labels.unknownTime
  const seconds = Math.floor((Date.now() - timestamp) / 1000)
  if (Math.abs(seconds) < 60) return labels.justNow
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })
  const minutes = Math.floor(seconds / 60)
  if (Math.abs(minutes) < 60) return formatter.format(-minutes, 'minute')
  const hours = Math.floor(seconds / 3600)
  return formatter.format(-hours, 'hour')
}
