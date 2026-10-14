// 「任务状态」显示名与色调的**唯一权威**。
//
// 为什么单独成文件：这两份映射原本在 `components/TaskCenter.vue` 与
// `features/v2/views/V2Tasks.vue` **各写了一份逐字相同**的实现。重复的代价已经真实出现：
// 两份**都漏了 `ACCEPTED`**，而它是真实状态 ⇒ 该状态回退到 `?? status`，
// 于是原始枚举字符串 `"ACCEPTED"` 被**直接显示给用户**。
//
// ⚠️ 这里的取值是**两个后端枚举的并集**，不是单一枚举：
// - `RunStatus`（domain/operation.rs:15-26，10 值，含 `ACCEPTED`）
// - `OperationStatus`（同文件 :74-81，含 `PARTIAL`）
// 任务列表展示的是"操作及其当前 run"，两种值都可能出现。
import { useLocale } from '@/shared/composables/useLocale'

// 注意：`t()` 必须在**函数体内**调用。若把 t(...) 的结果存进模块级常量，
// 它只会在 import 时按当时的语言算一次，之后切换语言不会更新。
const { t } = useLocale()

export type StatusTone = 'success' | 'error' | 'neutral' | 'warning' | 'info'

/**
 * 任务状态的显示名。未识别取值**原样返回**（保持既有回退语义，便于暴露新状态）。
 */
export const taskStatusLabel = (status: string): string => {
  switch (status) {
    case 'QUEUED':
      return t('status.queued')
    case 'PREPARING':
      return t('status.preparing')
    case 'SUBMITTING':
      return t('status.submitting')
    case 'SUBMISSION_UNCERTAIN':
      return t('status.submissionUncertain')
    case 'ACCEPTED':
      return t('status.accepted')
    case 'RUNNING':
      return t('status.running')
    case 'RETRY_WAIT':
      return t('status.retryWait')
    case 'SUCCEEDED':
      return t('status.success')
    case 'FAILED':
      return t('status.failed')
    case 'CANCELLED':
      return t('status.cancelled')
    case 'PARTIAL':
      return t('status.partial')
    default:
      return status
  }
}

/** 任务状态的徽标色调。未识别取值回退为 `info`。 */
export const taskStatusTone = (status: string): StatusTone => {
  if (status === 'SUCCEEDED') return 'success'
  if (status === 'FAILED') return 'error'
  if (status === 'CANCELLED') return 'neutral'
  if (status === 'PARTIAL' || status === 'SUBMISSION_UNCERTAIN' || status === 'RETRY_WAIT')
    return 'warning'
  return 'info'
}
