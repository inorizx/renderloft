// 「它为什么没在动」在界面上的**唯一权威**（显示名 / 色调 / 要不要人动手）。
//
// 为什么单独成文件：这份映射有两个消费者（任务卡片的角标与详情弹窗），而重写第二份的代价
// 在本仓库真实发生过——`taskStatusLabel` 一度在 `TaskCenter.vue` 与 `V2Tasks.vue` 里各写一遍，
// 两份**都漏了 `ACCEPTED`**，于是原始枚举字符串被直接显示给用户。
//
// ⚠️ 原因码是**后端的机器契约**（`domain::operation::ATTENTION_LADDER`），不是散文：
// 后端那两句 `message` / `deadlineEffect` 是中文原文（给 CLI / MCP / Agent 用的），
// 界面**不直接显示**它们——否则英文界面里会蹦出中文。界面按原因码取自己的 i18n 文案。
import { useLocale } from '@/shared/composables/useLocale'
import type { StatusTone } from '@/shared/lib/taskStatus'

// 注意：`t()` 必须在**函数体内**调用（模块级常量只会在 import 时算一次，切语言不会更新）。
const { t } = useLocale()

/** 原因码 → 界面显示名。未识别的取值**原样返回**（暴露新状态，而不是显示空白）。 */
export const attentionReasonLabel = (reason: string): string => {
  switch (reason) {
    case 'SUBMISSION_UNCERTAIN':
      return t('attention.uncertain')
    case 'RETRY_BACKOFF':
      return t('attention.retryBackoff')
    case 'SUBMITTING':
      return t('attention.submitting')
    case 'QUEUED':
      return t('attention.queued')
    default:
      return reason
  }
}

/** 原因码 → 色调。「等确认」最重（可能已经花了钱却不知道结果），其余是"在等"，都用 warning。 */
export const attentionTone = (reason: string): StatusTone => {
  switch (reason) {
    case 'SUBMISSION_UNCERTAIN':
    case 'RETRY_BACKOFF':
      return 'warning'
    default:
      return 'info'
  }
}

/**
 * 「它为什么没在动」的一句解释——**界面自己的文案**（不显示后端那句中文）。
 *
 * `hasDeadline` 决定后半句：有死线的档位说清"到点会判失败"，没有死线的（排队、退避重试）
 * **不许**说"到点会怎样"——那会让人去等一个不会发生的事件（后端为此专门改过一次）。
 */
export const attentionExplanation = (reason: string, hasDeadline: boolean): string => {
  switch (reason) {
    case 'SUBMISSION_UNCERTAIN':
      return hasDeadline ? t('attention.whyUncertain') : t('attention.whyUncertainNoDeadline')
    case 'RETRY_BACKOFF':
      return t('attention.whyRetryBackoff')
    case 'SUBMITTING':
      return t('attention.whySubmitting')
    case 'QUEUED':
      return t('attention.whyQueued')
    default:
      return ''
  }
}

/**
 * 这个原因码**现在能做什么**？界面只认它自己做得到的两件（取消 / 重试）；
 * `operation.get` / `operation.wait` 这类是给 CLI 与 Agent 的，界面不需要按钮。
 */
export const attentionOffersRetry = (nextActions: string[] | undefined): boolean =>
  (nextActions ?? []).includes('operation.retry')
