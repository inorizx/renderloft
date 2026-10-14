// 配方「类型」的**唯一权威**定义。
//
// 为什么单独成文件：这条规则曾有两份实现——资源库（V2Recipes）按 `driverKey` 正确分派，
// 首页（Home）却写成硬编码常量、还丢掉了入参，于是 ComfyUI / OpenAI 的配方在首页
// 一律显示成「RH 工作流」。同一规则留两处，迟早打架；这里收成一份，两边共用。
//
// ⚠️ 2026-10-12 改过一次判据：原来按 `driverKey` 判（只能说"在哪跑"），现在按后端给的
// `kind` 判（说"这是什么"）。原因是卡片上"类型 + 连接名"把同一件事说了两遍，而用户认自己的
// 东西靠的是"我那个 RH 应用""我导进来的那个工作流"。`kind` 由后端从**绑定的形状**派生
// （`webappId` / `workflowId` / 内联 `workflow`），判据只留一份、留在他那边；
// 前端仍保留按 `driverKey` 的退路，因为首页的"最近使用"是从旧响应拼的。
import type { RecipeSummary } from '@/shared/api/v2'
import { useLocale } from '@/shared/composables/useLocale'

// `t` 读的是模块级 locale ref，setup 之外调用也成立（与 api 层同款用法）。
const { t } = useLocale()

/** 配方类型判别键，与资源库的类型筛选选项一一对应（`'all'` 只是筛选项，不算类型）。 */
export type RecipeTypeKey = 'rh-app' | 'rh-workflow' | 'comfyui-workflow' | 'openai-image' | 'other'

/**
 * 由配方派生类型键。**优先后端给的 `kind`**，没有才退回按 `driverKey` 猜。
 *
 * 退路为什么要留着：`kind` 是 2026-10-12 才加进 summary 的，而首页的最近使用列表、
 * 以及任何旧缓存响应都不带它——那时候宁可退回"按驱动说"，也不要显示成「其他」。
 *
 * `driverKey` 自身有两种形态（导入写裸键、UI 高级创建写 `vendor/protocol`），
 * 所以取末段协议名，两种都归一。
 */
export const recipeTypeKey = (recipe: Pick<RecipeSummary, 'kind' | 'driverKey'>): RecipeTypeKey => {
  switch (recipe.kind) {
    case 'rh-app':
    case 'rh-workflow':
    case 'comfyui-workflow':
    case 'openai-image':
      return recipe.kind
  }
  const protocol = (recipe.driverKey ?? '').split('/').pop() ?? ''
  switch (protocol) {
    case 'runninghub':
      // 没有 `kind` 时只能说到驱动这一层：RunningHub 上应用与工作流都有，
      // 这里选"工作流"是因为它涵盖面更大（老数据里工作流占多数），且不会把工作流说成应用。
      return 'rh-workflow'
    case 'comfyui':
      return 'comfyui-workflow'
    case 'openai-images':
      return 'openai-image'
    default:
      return 'other'
  }
}

/** 配方类型的中英文显示名。说的是"**这是什么**"，不是"在哪跑"。 */
export const recipeTypeLabel = (recipe: Pick<RecipeSummary, 'kind' | 'driverKey'>): string => {
  switch (recipeTypeKey(recipe)) {
    case 'rh-app':
      return t('library.kindRhApp')
    case 'rh-workflow':
      return t('library.kindRhWorkflow')
    case 'comfyui-workflow':
      return t('library.kindComfyWorkflow')
    case 'openai-image':
      return t('library.kindOpenAiImage')
    default:
      return t('library.kindOther')
  }
}
