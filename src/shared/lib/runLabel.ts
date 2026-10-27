// 「这批结果里哪张对应哪条输入」——预览框里每一项的标签来源。
//
// 为什么单独成文件：两个消费者（任务中心的跨任务画廊、任务页的预览框）都要它，而**第二份实现
// 就是漂移的开始**（本仓库真实发生过：`taskStatusLabel` 一度在两处各写一遍，两份都漏了 `ACCEPTED`）。
//
// 背景（产品愿景「批量是核心能力」里那句"**结果按输入分组查看**"）：产物本来就是按 run 取的
// （`listArtifacts(runId)`），摊进预览框的顺序也已经是 run 顺序 —— 也就是说**分组其实一直在**，
// 只是每一项贴的标签是**任务名**，于是"12 张图里哪 3 张来自第 2 条提示词"没法回答。
// 这里只补那个标签，不动任何取数与布局。
import { LONG_TEXT_HINTS } from './fieldHeuristics'

/**
 * 后端**自动生成**的 `clientRunId` 形态：
 * * `run-<序号>` —— 调用方没起名字时按序号补的（`v2/domain/operation.rs::auto_client_run_id`）；
 * * `run-<32 位十六进制>` —— 更早的默认值（uuid），老数据里还有。
 *
 * 两种都要认出来，因为标签的取舍顺序依赖它：调用方**自己**给的 `clientRunId` 是它的标签，优先用；
 * 而自动生成的那个对"这是哪条输入"一个字都没说（它只是库里的唯一标识，后端也不会拿它当
 * 产物文件名），不如输入摘要。
 */
const AUTO_CLIENT_RUN_ID = /^run-(\d+|[0-9a-f]{32})$/

export const isAutoClientRunId = (clientRunId: string) => AUTO_CLIENT_RUN_ID.test(clientRunId)

/** 摘要长度：够认出是哪条输入，又不至于把标签撑成一段正文。 */
const DIGEST_LIMIT = 28

const digestOf = (value: string) => {
  const flattened = value.trim().replace(/\s+/g, ' ')
  return flattened.length > DIGEST_LIMIT ? `${flattened.slice(0, DIGEST_LIMIT)}…` : flattened
}

/**
 * 输入里最值得当标签的那个字符串。
 *
 * 优先名字"像提示词/描述"的字段（复用 `fieldHeuristics` 的 `LONG_TEXT_HINTS` —— **不另写一份
 * 启发式**，否则表单那边认长文本、这边认别的字段，同一批任务会有两种说法）；没有就取第一个
 * 非空字符串。全是文件/数值这类没有可读文本的输入时返回 `undefined`（**不编一个标签**）。
 */
export const inputDigest = (inputs?: Record<string, unknown>): string | undefined => {
  if (!inputs) return undefined
  const entries = Object.entries(inputs).filter(
    ([, value]) => typeof value === 'string' && value.trim().length > 0
  )
  if (!entries.length) return undefined
  const preferred = entries.find(([key]) =>
    LONG_TEXT_HINTS.some((hint) => key.toLowerCase().includes(hint))
  )
  return digestOf(String((preferred ?? entries[0])[1]))
}

/**
 * 一条 run 在结果里的**分组标签**。判据从"最能说明是哪条输入"排到"只能说这是第几条"：
 *
 * 1. 调用方自己给的 `clientRunId`（不是自动生成的那种）—— 那是**它**给这条起的名字；
 * 2. 输入摘要（见 `inputDigest`）；
 * 3. `run#<序号>` —— 只剩位置信息时，就如实说位置（与批次详情里同一套写法）。
 */
export const runGroupLabel = (run: {
  clientRunId?: string
  sequence: number
  inputs?: Record<string, unknown>
}): string => {
  const clientRunId = (run.clientRunId ?? '').trim()
  if (clientRunId && !isAutoClientRunId(clientRunId)) return clientRunId
  return inputDigest(run.inputs) ?? `run#${run.sequence}`
}
