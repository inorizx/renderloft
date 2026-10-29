<script setup lang="ts">
/**
 * 字段选择列表：勾哪些字段暴露成表单、它们叫什么、用什么控件。
 *
 * 同一份 UI 服务两条导入路径（ComfyUI API Prompt、RunningHub 工作流）——它们的字段
 * 来自同一个解析器（RH 的工作流也会被拉成 ComfyUI 格式的 prompt），所以"怎么选"完全一样。
 * 分成两份会立刻漂移：改了一边忘了另一边，而用户看到的表单会长得不一样。
 *
 * ⚠️ 这里的 `rows` 是**可变的**：勾选/改名/换控件都直接改行对象（引用不变），
 * 父组件因此拿到同一份数据。没有 emit —— 复制一份再回传只会让两边短暂地不一致。
 */
import Badge from '@/components/ui/Badge.vue'
import Input from '@/components/ui/Input.vue'
import { useLocale } from '@/shared/composables/useLocale'
import type { FieldSelectionRow } from '@/shared/types/fieldSelection'

defineProps<{
  rows: FieldSelectionRow[]
  enrichment?: { classesUsed: number; classesKnown: number; reason: string | null } | null
}>()

const { t } = useLocale()
</script>

<template>
  <div class="space-y-3">
    <div class="max-h-80 space-y-2 overflow-y-auto pr-1">
      <div
        v-for="row in rows"
        :key="row.key"
        class="flex items-center gap-3 rounded-md border border-border px-3 py-2"
        :class="row.selected ? '' : 'opacity-60'"
      >
        <input
          :id="`field-${row.key}`"
          v-model="row.selected"
          type="checkbox"
          class="size-4 shrink-0 rounded border-input accent-primary"
        />
        <div class="min-w-0 flex-1">
          <label :for="`field-${row.key}`" class="block truncate text-sm font-medium">
            {{ row.fieldName }}
            <span class="ml-1 font-mono text-xs font-normal text-muted-foreground">
              {{ row.classType }} · {{ t('library.node', { id: row.nodeId }) }}
            </span>
          </label>
          <p
            v-if="row.default !== undefined && row.default !== null"
            class="truncate text-xs text-muted-foreground"
          >
            {{ t('library.currentValue', { value: String(row.default) }) }}
          </p>
        </div>
        <Badge v-if="row.media" variant="secondary" class="shrink-0">{{ t('library.file') }}</Badge>
        <!--
          **控件**：默认（推荐）= 不覆盖，让引擎声明/值的形状决定；用户一改就是他的决定。
          这件事只有用户知道：`aspect_ratio` 其实只有三种画幅、他常用的采样器就那五个。
          选了「下拉」才要他填候选项（没填就退回文本，不造空下拉）。
        -->
        <select
          v-model="row.widget"
          :disabled="!row.selected"
          :aria-label="t('library.widgetFor', { name: row.fieldName })"
          class="h-8 w-32 shrink-0 rounded-md border border-input bg-secondary px-2 text-xs"
        >
          <option value="">{{ t('library.widgetDefault') }}</option>
          <option value="text">{{ t('library.widgetText') }}</option>
          <option value="textarea">{{ t('library.widgetTextarea') }}</option>
          <option value="number">{{ t('library.widgetNumber') }}</option>
          <option value="switch">{{ t('library.widgetSwitch') }}</option>
          <option value="dropdown">{{ t('library.widgetDropdown') }}</option>
          <option value="file">{{ t('library.widgetFile') }}</option>
        </select>
        <Input
          v-if="row.widget === 'dropdown'"
          v-model="row.options"
          :disabled="!row.selected"
          :placeholder="t('library.widgetOptionsPlaceholder')"
          class="h-8 w-56 shrink-0 text-xs"
        />
        <Input
          v-model="row.label"
          :disabled="!row.selected"
          :placeholder="
            row.defaultLabel || t('library.fieldLabelPlaceholder', { name: row.fieldName })
          "
          class="h-8 w-44 shrink-0 text-xs"
        />
      </div>
    </div>
    <p v-if="enrichment && !enrichment.classesKnown" class="text-xs leading-5 text-warning">
      {{
        t('library.enrichmentMissing', {
          reason: enrichment.reason || t('library.enrichmentUnknown')
        })
      }}
    </p>
    <p v-else-if="enrichment" class="text-xs leading-5 text-muted-foreground">
      {{
        t('library.enrichmentOk', {
          known: enrichment.classesKnown,
          used: enrichment.classesUsed
        })
      }}
    </p>
  </div>
</template>
