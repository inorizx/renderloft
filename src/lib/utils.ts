import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

/** 供组件声明 `class` prop 用：与 `cn` 同一类型，调用点才能传条件数组。 */
export type { ClassValue }

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/**
 * 去掉富文本描述里的 HTML 标签与常见实体，避免 RunningHub 等平台返回的
 * `<p>`/`<h3>` 标签在纯文本展示中原样显示。
 */
export function stripHtml(input: string): string {
  if (!input) return input
  return input
    .replace(/<[^>]*>/g, ' ')
    .replace(/&nbsp;/gi, ' ')
    .replace(/&amp;/gi, '&')
    .replace(/&lt;/gi, '<')
    .replace(/&gt;/gi, '>')
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#x27;/gi, "'")
    .replace(/\s+/g, ' ')
    .trim()
}
