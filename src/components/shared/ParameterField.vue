<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { FileText, ImagePlus, LoaderCircle, X } from '@lucide/vue'
import { cn } from '@/lib/utils'
import { Input, Textarea, Button } from '@/components/ui'
import { useLocale } from '@/shared/composables/useLocale'
import { looksLikeLongText } from '@/shared/lib/fieldHeuristics'

export interface FieldSpec {
  key: string
  type?: string
  format?: string
  enum?: string[]
  description?: string
  required?: boolean
  /** 数字范围与步长（`minimum`/`maximum`/`multipleOf` 是 JSON Schema 的关键字，
   *  来源是平台或引擎声明的真实约束——比如 ComfyUI 节点定义的 min/max/step）。 */
  minimum?: number
  maximum?: number
  multipleOf?: number
}

interface Props {
  field: FieldSpec
  modelValue?: unknown
  /** 文件字段的当前上传态。 */
  file?: { filename?: string; uploading?: boolean } | null
  /** 父级显式注入的错误信息（优先级最高）。 */
  error?: string
  idPrefix?: string
  /** 字段被上层逻辑接管（如批量变量字段）时禁用编辑。 */
  disabled?: boolean
  class?: string
}

const props = withDefaults(defineProps<Props>(), { idPrefix: 'field' })

const emit = defineEmits<{
  // 本组件只从 Input/Textarea/Select（string）与复选框（boolean）转发，别再放宽成 unknown：
  // 父级拿到的值要能直接喂给 setValue，放宽只会把类型检查推给调用点。
  'update:modelValue': [value: string | boolean]
  'file-change': [key: string, file: File]
  'clear-file': [key: string]
}>()

const isBoolean = computed(() => props.field.type === 'boolean')
const isEnum = computed(() => !!props.field.enum?.length)
const isFile = computed(() => props.field.format === 'file')
const { t } = useLocale()

const isNumber = computed(() => props.field.type === 'integer' || props.field.type === 'number')
const isInteger = computed(() => props.field.type === 'integer')

/**
 * 数字的步长：**平台给了就用平台的**（ComfyUI 的 `step` / JSON Schema 的 `multipleOf`），
 * 没给才按整数/浮点猜。
 *
 * 这条以前是漏的：schema 里明明带着 `minimum`/`maximum`（RH 应用那条导入路一直在写，
 * 还有测试钉着 `142::width` 的 16–16384），但控件从来没读过——信息一路走到前端，
 * 最后一厘米没接。
 */
const numberStep = computed(() => {
  const step = props.field.multipleOf
  if (typeof step === 'number' && step > 0) return String(step)
  return isInteger.value ? '1' : 'any'
})

/** 普通字符串字段（非文件/枚举/布尔/数字）：统一用自适应高度的多行输入，
 * 默认软换行，短内容一行高、长内容自动展开，避免「猜单行/多行」判错。 */
const isLongField = computed(() => {
  // 平台显式声明为多行文本（ComfyUI multiline / RunningHub multiline 字段）时直接采用。
  if (props.field.format === 'textarea') return true
  // 关键词 / 描述长度启发式：唯一权威在 shared/lib/fieldHeuristics（V2Runner 共用一份）。
  return looksLikeLongText(props.field)
})

const displayValue = computed(() => (props.modelValue == null ? '' : String(props.modelValue)))
const booleanChecked = computed(() => props.modelValue === true)
const selectValue = computed(() => (props.modelValue == null ? '' : String(props.modelValue)))

const isEmptyValue = computed(
  () =>
    props.modelValue == null || (typeof props.modelValue === 'string' && !props.modelValue.trim())
)
const numberInvalid = computed(() => {
  if (!isNumber.value || isEmptyValue.value) return false
  const s = String(props.modelValue)
  return isInteger.value ? !/^-?\d+$/.test(s) : !Number.isFinite(Number(s))
})

const fieldError = computed(() => {
  if (props.error) return props.error
  if (props.field.required && isEmptyValue.value) return t('field.required')
  if (numberInvalid.value)
    return isInteger.value ? t('field.mustBeInteger') : t('field.mustBeNumber')
  return undefined
})

const id = computed(() => `${props.idPrefix}-${props.field.key}`)

// ---------- 文件字段：拖拽 + 缩略图预览 ----------
const isDragging = ref(false)
const pickedFile = ref<File | null>(null)
const pickedPreviewUrl = ref('')

const hasFile = computed(() => !!(props.file?.filename || pickedFile.value))
const previewUrl = computed(() =>
  pickedFile.value?.type.startsWith('image/') ? pickedPreviewUrl.value : ''
)

const formatSize = (bytes: number) => {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`
  return `${bytes} B`
}

const setPickedFile = (file: File | null) => {
  if (pickedPreviewUrl.value) URL.revokeObjectURL(pickedPreviewUrl.value)
  pickedFile.value = file
  pickedPreviewUrl.value = file ? URL.createObjectURL(file) : ''
}

const acceptFile = (file: File | null | undefined) => {
  if (!file || props.file?.uploading || props.disabled) return
  setPickedFile(file)
  emit('file-change', props.field.key, file)
}

const onInputChange = (event: Event) => {
  const input = event.target as HTMLInputElement
  acceptFile(input.files?.[0])
  input.value = ''
}

const onDrop = (event: DragEvent) => {
  isDragging.value = false
  acceptFile(event.dataTransfer?.files?.[0])
}

const removeFile = () => {
  setPickedFile(null)
  emit('clear-file', props.field.key)
}

// 父级换成了另一个文件（如会话恢复）时，丢弃本地预览避免张冠李戴。
watch(
  () => props.file?.filename,
  (filename) => {
    if (filename && pickedFile.value && pickedFile.value.name !== filename) {
      setPickedFile(null)
    }
  }
)

onBeforeUnmount(() => {
  if (pickedPreviewUrl.value) URL.revokeObjectURL(pickedPreviewUrl.value)
})

// ---------- 长文本自动增高 ----------
const multilineEl = ref<InstanceType<typeof Textarea> | null>(null)
const autoGrow = () => {
  const el = multilineEl.value?.$el as HTMLTextAreaElement | undefined
  if (!el) return
  el.style.height = 'auto'
  // 上限 24rem（384px），超出后内部滚动。
  el.style.height = `${Math.min(el.scrollHeight, 384)}px`
}
watch(displayValue, () => nextTick(autoGrow))
onMounted(() => nextTick(autoGrow))
</script>

<template>
  <div :class="cn('space-y-1.5', isLongField && 'sm:col-span-2', props.class)">
    <template v-if="isBoolean">
      <div class="flex items-center gap-2 text-sm">
        <input
          :id="id"
          type="checkbox"
          :checked="booleanChecked"
          :disabled="disabled"
          class="size-4 rounded border-input accent-primary"
          @change="emit('update:modelValue', ($event.target as HTMLInputElement).checked)"
        />
        <label :for="id">
          {{ field.description || field.key }}
          <span v-if="field.required" class="ml-1 text-destructive" aria-hidden="true">*</span>
          <span v-if="field.required" class="sr-only">{{ t('field.requiredMark') }}</span>
        </label>
        <span class="ml-auto"><slot name="actions" /></span>
      </div>
      <p v-if="fieldError" class="text-xs leading-5 text-destructive">{{ fieldError }}</p>
    </template>

    <template v-else>
      <div class="flex items-center justify-between gap-2">
        <label :for="id" class="text-sm font-medium">
          {{ field.description || field.key }}
          <span v-if="field.required" class="ml-1 text-destructive" aria-hidden="true">*</span>
          <span v-if="field.required" class="sr-only">{{ t('field.requiredMark') }}</span>
        </label>
        <slot name="actions" />
      </div>

      <template v-if="isFile">
        <div
          class="rounded-md border border-dashed transition-colors"
          :class="[
            isDragging ? 'border-ring bg-accent' : 'border-input bg-secondary/50',
            hasFile ? 'p-2.5' : 'p-4'
          ]"
          @dragover.prevent="isDragging = true"
          @dragleave.prevent="isDragging = false"
          @drop.prevent="onDrop"
        >
          <input
            :id="id"
            type="file"
            class="sr-only"
            :disabled="file?.uploading || disabled"
            @change="onInputChange"
          />
          <div v-if="hasFile" class="flex items-center gap-3">
            <div
              class="relative size-14 shrink-0 overflow-hidden rounded-md border border-border bg-background"
            >
              <img
                v-if="previewUrl"
                :src="previewUrl"
                :alt="file?.filename ?? pickedFile?.name ?? t('field.selectedFile')"
                class="size-full object-cover"
              />
              <div v-else class="grid size-full place-items-center text-muted-foreground">
                <FileText class="size-5" />
              </div>
              <div
                v-if="file?.uploading"
                class="absolute inset-0 grid place-items-center bg-background/70"
              >
                <LoaderCircle class="size-4 animate-spin" />
              </div>
            </div>
            <div class="min-w-0 flex-1">
              <p class="truncate text-xs font-medium">
                {{ file?.filename ?? pickedFile?.name }}
              </p>
              <p class="mt-0.5 text-xs text-muted-foreground">
                {{
                  file?.uploading
                    ? t('field.uploading')
                    : file?.filename
                      ? t('field.uploaded')
                      : pickedFile
                        ? formatSize(pickedFile.size)
                        : ''
                }}
              </p>
            </div>
            <Button
              variant="ghost"
              size="sm"
              class="h-7 shrink-0 gap-1 px-2 text-xs"
              :disabled="file?.uploading"
              @click="removeFile"
            >
              <X class="size-3.5" />{{ t('field.remove') }}
            </Button>
          </div>
          <label
            v-else
            :for="id"
            :class="[
              'flex flex-col items-center justify-center gap-1.5 text-center',
              disabled ? 'cursor-not-allowed opacity-60' : 'cursor-pointer'
            ]"
          >
            <ImagePlus class="size-5 text-muted-foreground" />
            <p class="text-xs text-muted-foreground">
              <span class="font-medium text-foreground">{{ t('field.clickToUpload') }}</span
              >{{ t('field.dragHint') }}
            </p>
          </label>
        </div>
      </template>

      <template v-else-if="isEnum">
        <select
          :id="id"
          class="h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          :value="selectValue"
          :disabled="disabled"
          @change="emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
        >
          <option v-for="option in field.enum" :key="option" :value="option">
            {{ option }}
          </option>
        </select>
      </template>

      <Input
        v-else-if="isNumber"
        :id="id"
        type="number"
        :step="numberStep"
        :min="field.minimum"
        :max="field.maximum"
        :placeholder="field.description"
        :model-value="displayValue"
        :disabled="disabled"
        @update:model-value="(value) => emit('update:modelValue', value)"
      />

      <Textarea
        v-else
        ref="multilineEl"
        :id="id"
        :model-value="displayValue"
        :placeholder="field.description"
        :rows="isLongField ? 3 : 1"
        :disabled="disabled"
        :class="isLongField ? 'min-h-20' : 'min-h-9'"
        @update:model-value="(value) => emit('update:modelValue', value)"
      />

      <p v-if="fieldError" class="text-xs leading-5 text-destructive">{{ fieldError }}</p>
    </template>
  </div>
</template>
