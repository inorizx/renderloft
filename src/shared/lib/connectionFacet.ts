// 连接的**状态面**：这条连接现在能不能跑。
//
// 面的归属是**平台**，不是 RenderLoft。RunningHub 的 OpenAPI 里有两个面——`accountStatus`
// （余额：币 + 可选法币）与 `queue/status`（并发/排队）；本地 ComfyUI 一个都没有（它有队列，
// 但我们还没接），OpenAI 官方也不提供余额端点。所以这个文件只做两件事：
//
// 1. **只转述平台报的数**（与 `runUsage.ts` 同一条判据）：不换算、不聚合、不编造 0；
// 2. 分清「**平台不提供**」与「**这一次查不到**」——前者什么都不显示（那是常态，不是故障），
//    后者显示「不可读」（那是故障）。混成一句话，用户就会以为连接坏了。
//
// 单位（币）是平台的计价单位，拼写在 `unit.coins` 一处，别处引用。**应用级词表里不该出现
// 带厂商缩写的条目**（那里曾经有一条「RH 币」：全站连接叫 RunningHub，只有那一格缩写成 RH）。
import type { AccountStatus, QueueStatus } from '@/shared/types/runninghub'
import { useLocale } from '@/shared/composables/useLocale'

const { t } = useLocale()

/**
 * 平台报的数可能缺席（daemon 把平台的 JSON 原样转过来，类型只是前端的假设），
 * 所以入参用 `Partial`：字段没有就是没有，不按"类型说有"去读。
 */
type AccountFacetInput = Partial<AccountStatus> | null | undefined
type QueueFacetInput = Partial<QueueStatus> | null | undefined

/**
 * 钱数：整数加千位分隔（`12345` → `12,345`），**小数原样转述**。
 *
 * 小数不重新格式化是有意的：`2.160` 解析成浮点再打印会动到精度，而这是平台的对账数字。
 * 非数字也原样（脏数据不猜、不显示成 0）。
 */
const formatAmount = (raw: string): string => {
  const value = Number(raw)
  if (!Number.isFinite(value)) return raw
  return Number.isInteger(value) ? value.toLocaleString() : raw
}

/** 余额一行：「12,345 币 · CNY 2.160」。平台一项都没报就返回 null（不编造）。 */
export const formatAccountFacet = (status: AccountFacetInput): string | null => {
  if (!status) return null
  const parts: string[] = []
  const coins = status.remainCoins?.trim()
  if (coins) parts.push(t('unit.coins', { value: formatAmount(coins) }))
  const money = status.remainMoney?.trim()
  if (money) parts.push(`${status.currency?.trim() ?? ''} ${formatAmount(money)}`.trim())
  return parts.length ? parts.join(' · ') : null
}

/** 队列一行：「并发 1/1 · 排队 0」。平台一项都没报就返回 null。 */
export const formatQueueFacet = (status: QueueFacetInput): string | null => {
  if (!status) return null
  const parts: string[] = []
  const running = status.runningCount?.trim()
  const limit = Number(status.concurrentLimit)
  if (running) {
    parts.push(
      Number.isFinite(limit) && limit > 0
        ? t('connections.facetRunningOf', { running, limit: String(limit) })
        : t('connections.facetRunning', { running })
    )
  }
  const queued = status.queuedCount?.trim()
  if (queued) parts.push(t('connections.facetQueued', { queued }))
  return parts.length ? parts.join(' · ') : null
}
