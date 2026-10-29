/**
 * 字段选择行的形状：源自后端的字段预览 + 用户在这条字段上的三个决定
 * （勾不勾、叫什么、用什么控件）。
 *
 * 放在 `shared/` 而不是页面里：两条导入路径（ComfyUI API Prompt、RunningHub 工作流）
 * 与共用组件 `FieldSelectionList.vue` 都认这一个形状。分开定义会立刻漂移——
 * 一边加了字段另一边不知道，而用户看到的表单会长得不一样。
 */
export interface FieldSelectionRow {
  /** schema 里的键，形如 `475::image`。 */
  key: string
  nodeId: string
  classType: string
  fieldName: string
  default?: unknown
  /** 平台把它声明成媒体（图片/音频）——表单里会是上传控件。 */
  media: boolean
  /** 不写备注名时它会叫什么（`节点标题 · 字段名`）——当占位提示用。 */
  defaultLabel: string
  /** 勾了才进表单；没勾的字段不进 schema，但**底层工作流一个字段都不少**。 */
  selected: boolean
  /** 用户写的中文备注名。空串 = 用 `defaultLabel`。 */
  label: string
  /** 用户选的控件；空串 = 默认（不覆盖，让引擎声明/值的形状决定）。 */
  widget: string
  /** 选了「下拉」时他填的候选项（逗号分隔的原文）。 */
  options: string
}
