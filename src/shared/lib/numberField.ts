/**
 * 数字字段的**唯一判据**：类型、范围、步长。
 *
 * 为什么抽出来：`ParameterField`（字段下方红字）与 `V2Runner`（提交前校验）各判一次的话，
 * 迟早会出现"字段说没问题、点生成却被拦"（或反过来）。后端 `BasicSchemaValidator` 是
 * 最终权威，这里只是把同一条判据提前到**花钱之前**，并把范围显示给用户。
 */
export interface NumberConstraints {
  type?: string
  minimum?: number
  maximum?: number
  multipleOf?: number
}

/** `undefined` = 通过；其余是问题类型。 */
export type NumberIssue = 'type' | 'range'

export function numberIssue(field: NumberConstraints, value: unknown): NumberIssue | undefined {
  const isInteger = field.type === 'integer'
  if (!isInteger && field.type !== 'number') return undefined
  if (value === undefined || value === null) return undefined
  const raw = String(value).trim()
  if (raw === '') return undefined
  if (isInteger && !/^-?\d+$/.test(raw)) return 'type'
  const numeric = Number(raw)
  if (!Number.isFinite(numeric)) return 'type'
  if (field.minimum !== undefined && numeric < field.minimum) return 'range'
  if (field.maximum !== undefined && numeric > field.maximum) return 'range'
  // ⚠️ `multipleOf` **刻意不在这里拦**：daemon 的 `BasicSchemaValidator` 只校验类型与
  // min/max，不校验步长；前端若更严，就会出现"界面不让填、直接调 API 却可以"的分叉。
  // 它仍然作为 `<input step>` 的属性存在（控件箭头按它走），只是不当硬约束。
  return undefined
}

/**
 * 范围提示（`undefined` = 没有可说的约束）。
 *
 * 用 `≥ / ≤ / –` 而不是自然语言：中英两种界面都不需要翻译，也不会把"16–16384"
 * 翻成一句更长的废话。
 */
export function numberRangeText(field: NumberConstraints): string | undefined {
  const { minimum, maximum } = field
  if (minimum !== undefined && maximum !== undefined) return `${minimum}–${maximum}`
  if (minimum !== undefined) return `≥ ${minimum}`
  if (maximum !== undefined) return `≤ ${maximum}`
  return undefined
}
