// 秒数 → 人话时长（「27 秒」/「1 分 9 秒」/「1 小时 2 分」）。
//
// 单独成文件是因为它原来有**两份实现**：任务中心的"已运行多久"与"总共花了多久"各写了一遍
// 同样的 60/3600 分段。加第三处（平台报的任务耗时）时就该收成一份了——同一规则留两处，迟早打架。
import { useLocale } from '@/shared/composables/useLocale'

const { t } = useLocale()

/**
 * 把**秒数**格式化成时长文案。
 *
 * 负数与 NaN 一律当 0（调用方给的多半是"现在 − 开始"或平台报的数，两者都可能是脏的）；
 * 不足 1 秒显示「0 秒」而不是空串——调用方已经把"不知道"用别的方式表达过了（`task.unknownTime`）。
 */
export const formatSeconds = (seconds: number): string => {
  const safe = Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : 0
  if (safe < 60) return t('task.durationSeconds', { n: safe })
  const minutes = Math.floor(safe / 60)
  if (minutes < 60) return t('task.durationMinSec', { m: minutes, s: safe % 60 })
  return t('task.durationHourMin', { h: Math.floor(minutes / 60), m: minutes % 60 })
}
