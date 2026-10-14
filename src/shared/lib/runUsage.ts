// 一次运行的**用量**：耗时与花费。
//
// 判据是"只转述平台自己报的数"：RunningHub 的输出里就带着 `{consumeCoins, taskCostTime}`
// （两者在**同一行**，因为平台**按时长计费**），后端原样存进 `usage_json`，这里只负责显示。
// 取不到就**不显示**——本地 ComfyUI 没有计费概念，硬凑一个"0 币"是编造。
import { useLocale } from '@/shared/composables/useLocale'
import { formatSeconds } from '@/shared/lib/duration'

const { t } = useLocale()

/** `usage` 的形状（与后端 `usage_json` 一致；`taskCostTime` 是**字符串**秒，批量会求和）。 */
export interface RunUsage {
  unit?: string
  amount?: string
  taskCostTime?: string
}

export interface RunUsageLike {
  usage?: RunUsage | null
}

/**
 * 把若干条 run 的用量合并成一行：「27 秒 · 6 币」。都没有就返回空串。
 *
 * 合并（而不是取第一条）是因为**批量**：后端已经把同一任务的多张输出按平台报的数求和，
 * 这里再把多条 run 相加，得到的才是这次操作的总账。
 *
 * 脏数据按"不知道"处理：`taskCostTime` 平台偶尔报 `"unknown"`，`Number()` 之后是 NaN，
 * 不参与求和、也不会显示成 0 秒。
 */
export const formatUsage = (runs: RunUsageLike[]): string => {
  let seconds = 0
  let coins = 0
  let hasCoins = false
  for (const run of runs) {
    const usage = run.usage
    if (!usage) continue
    const duration = Number(usage.taskCostTime)
    if (Number.isFinite(duration) && duration > 0) seconds += duration
    const amount = Number(usage.amount)
    if (usage.unit === 'coins' && Number.isFinite(amount)) {
      coins += amount
      hasCoins = true
    }
  }
  const parts: string[] = []
  if (seconds > 0) parts.push(formatSeconds(seconds))
  if (hasCoins) parts.push(t('unit.coins', { value: String(coins) }))
  return parts.join(' · ')
}
