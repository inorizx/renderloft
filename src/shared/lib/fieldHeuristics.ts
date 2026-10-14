/**
 * 字段「看起来是长文本」的启发式 —— 单一权威位置。
 *
 * 为什么单独成文件：这条启发式曾在两处各写一份，靠注释「与 ParameterField 的长文本
 * 启发式保持一致」人工同步（`ParameterField.vue` 决定字段要不要占满整行、输入框多高；
 * `V2Runner.vue` 决定批量表格的单元格用输入框还是文本域）。重复的地方迟早打架 ——
 * 与本仓库 `shared/lib/recipeType.ts` 收编同款问题(首页 vs 资源库各写一份类型判别)先例一致。
 *
 * ⚠️ 这里只收**公共内核**，两处的守卫策略**有意不同**，不要合并：
 * - `ParameterField` 问「这个字段要不要占满一行」—— 对所有类型都适用，不排除任何类型；
 * - `V2Runner` 问「这个单元格该渲染成文本域吗」—— 必须先排除 file / enum / boolean / number，
 *   否则数字字段也会被渲染成大号文本域。
 * 若把 V2Runner 的守卫挪进这里，ParameterField 的 `sm:col-span-2` 行为会跟着变。
 */

/**
 * 命中即视为长文本的关键词。
 *
 * ⚠️ 匹配的是**字段名与描述**（来自平台 schema 的数据），**不是 UI 文案**，
 * 所以这张表不进 i18n 词条 —— 改它不会影响任何界面语言。
 */
export const LONG_TEXT_HINTS = ['prompt', '提示词', '描述', '负面', 'negative', 'story', '脚本']

/** 关键词之外的兜底：描述达到该长度也视为长文本。 */
export const LONG_TEXT_DESCRIPTION_LENGTH = 28

interface FieldHintSource {
  key: string
  description?: string
}

/**
 * 字段「描述 + 名称」的小写文本。
 *
 * 名称（`key`）必须一起参与匹配：有些平台的描述为空，只有 `negative_prompt`
 * 这类 key 能表意。
 */
export const fieldHintText = (field: FieldHintSource) =>
  `${field.description ?? ''} ${field.key}`.toLowerCase()

/**
 * 该字段看起来是不是长文本（命中关键词，或描述足够长）。
 *
 * 调用方**自己**叠加类型守卫 —— 见文件头说明。
 */
export const looksLikeLongText = (field: FieldHintSource) =>
  LONG_TEXT_HINTS.some((hint) => fieldHintText(field).includes(hint)) ||
  (field.description?.length ?? 0) >= LONG_TEXT_DESCRIPTION_LENGTH
