<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronLeft, ChevronRight, Download, LoaderCircle, X } from '@lucide/vue'
import { Button, Card, Dialog, IconButton } from '@/components/ui'
import { useLocale } from '@/shared/composables/useLocale'
import type { TaskResult } from '@/shared/types/runninghub'

interface Props {
  open: boolean
  title: string
  results: TaskResult[]
  gallery?: PreviewItem[]
  initialIndex?: number
  downloadingIndex?: number
}

interface PreviewItem {
  result: TaskResult
  label?: string
}

const props = withDefaults(defineProps<Props>(), {
  gallery: () => [],
  initialIndex: 0,
  downloadingIndex: -1
})

const { t } = useLocale()
const emit = defineEmits<{
  'update:open': [value: boolean]
  'update:index': [value: number]
  download: [result: TaskResult, index: number]
}>()

const selectedIndex = ref(0)
const previewVideo = ref<HTMLVideoElement | HTMLVideoElement[] | null>(null)
const previewItems = computed<PreviewItem[]>(() =>
  props.gallery.length ? props.gallery : props.results.map((result) => ({ result }))
)
const resultKind = (result: TaskResult) => {
  const type = result.outputType?.toLowerCase() ?? ''
  const url = result.url?.toLowerCase() ?? ''
  if (type.includes('video') || /\.(mp4|webm|mov|m4v)(\?|$)/.test(url)) return 'video'
  if (type.includes('image') || /\.(png|jpe?g|webp|gif|avif)(\?|$)/.test(url)) return 'image'
  return 'text'
}
const isMediaPreview = computed(
  () =>
    previewItems.value.length > 0 &&
    previewItems.value.every(({ result }) => ['image', 'video'].includes(resultKind(result)))
)
const visibleResults = computed(() =>
  isMediaPreview.value
    ? [{ ...previewItems.value[selectedIndex.value], index: selectedIndex.value }]
    : previewItems.value.map((item, index) => ({ ...item, index }))
)
const dialogClass = computed(
  () =>
    `flex max-h-[90vh] flex-col overflow-hidden ${
      isMediaPreview.value ? 'result-preview-single max-w-none' : 'max-w-3xl'
    }`
)
const dialogStyle = computed(() => {
  return { width: `${Math.min(window.innerWidth * 0.9, 672)}px` }
})
const playVideo = (event: Event) => {
  const video = event.currentTarget as HTMLVideoElement
  video.muted = false
  void video.play().catch(() => {
    // Some browsers block asynchronous autoplay with sound; keep the preview usable.
    video.muted = true
    void video.play().catch(() => undefined)
  })
}
const handleVideoMetadata = (event: Event) => {
  playVideo(event)
}
const pausePreviewVideo = () => {
  const video = Array.isArray(previewVideo.value) ? previewVideo.value[0] : previewVideo.value
  video?.pause()
}
const handleVisibilityChange = () => {
  if (document.hidden) pausePreviewVideo()
}
const close = (open: boolean) => {
  if (!open) {
    pausePreviewVideo()
    selectedIndex.value = Math.max(0, Math.min(props.initialIndex, previewItems.value.length - 1))
  }
  emit('update:open', open)
}
const previousResult = () => {
  if (previewItems.value.length < 2) return
  pausePreviewVideo()
  selectedIndex.value =
    (selectedIndex.value - 1 + previewItems.value.length) % previewItems.value.length
  emit('update:index', selectedIndex.value)
}
const nextResult = () => {
  if (previewItems.value.length < 2) return
  pausePreviewVideo()
  selectedIndex.value = (selectedIndex.value + 1) % previewItems.value.length
  emit('update:index', selectedIndex.value)
}
const handleKeydown = (event: KeyboardEvent) => {
  if (!props.open || !isMediaPreview.value || previewItems.value.length < 2) return
  if (event.key === 'ArrowLeft') {
    event.preventDefault()
    previousResult()
  } else if (event.key === 'ArrowRight') {
    event.preventDefault()
    nextResult()
  }
}
onMounted(() => {
  window.addEventListener('keydown', handleKeydown)
  window.addEventListener('blur', pausePreviewVideo)
  document.addEventListener('visibilitychange', handleVisibilityChange)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown)
  window.removeEventListener('blur', pausePreviewVideo)
  document.removeEventListener('visibilitychange', handleVisibilityChange)
})
const clampIndex = (index: number) => Math.max(0, Math.min(index, previewItems.value.length - 1))
watch(
  () => props.open,
  (open) => {
    if (open) selectedIndex.value = clampIndex(props.initialIndex)
    else pausePreviewVideo()
  },
  { immediate: true }
)
watch(
  () => props.initialIndex,
  (initialIndex) => {
    if (!props.open) selectedIndex.value = clampIndex(initialIndex)
  }
)
watch(
  () => previewItems.value.length,
  (length) => {
    if (!props.open && length) selectedIndex.value = clampIndex(props.initialIndex)
  }
)
</script>

<template>
  <Dialog
    :open="open"
    :title="title"
    :class="`${dialogClass} result-preview-dialog`"
    :body-class="
      isMediaPreview ? 'result-preview-body result-preview-media-body' : 'result-preview-body'
    "
    :mobile-fullscreen="isMediaPreview"
    :frameless="isMediaPreview"
    :animated="!isMediaPreview"
    :style="dialogStyle"
    @update:open="close"
  >
    <div v-if="isMediaPreview" class="result-preview-toolbar">
      <IconButton
        :label="t('previewDialog.downloadResult', { n: selectedIndex + 1 })"
        variant="outline"
        :disabled="downloadingIndex === selectedIndex"
        @click="emit('download', previewItems[selectedIndex].result, selectedIndex)"
        ><LoaderCircle
          v-if="downloadingIndex === selectedIndex"
          class="size-4 animate-spin" /><Download v-else class="size-4"
      /></IconButton>
      <IconButton :label="t('previewDialog.closePreview')" variant="outline" @click="close(false)"
        ><X class="size-4"
      /></IconButton>
    </div>
    <span v-if="isMediaPreview && previewItems.length > 1" class="result-preview-count">
      {{ selectedIndex + 1 }} / {{ previewItems.length }}
    </span>
    <IconButton
      v-if="isMediaPreview && previewItems.length > 1"
      class="result-preview-nav result-preview-nav-previous"
      :label="t('previewDialog.prevResult')"
      variant="outline"
      @click.stop="previousResult"
      ><ChevronLeft class="size-5" />
    </IconButton>
    <IconButton
      v-if="isMediaPreview && previewItems.length > 1"
      class="result-preview-nav result-preview-nav-next"
      :label="t('previewDialog.nextResult')"
      variant="outline"
      @click.stop="nextResult"
      ><ChevronRight class="size-5" />
    </IconButton>
    <div class="result-preview-grid">
      <Card
        v-for="{ result, index } in visibleResults"
        :key="`${result.nodeId ?? 'result'}-${index}`"
        :class="[
          'result-preview-card overflow-hidden rounded-none shadow-none',
          isMediaPreview && 'result-preview-single-card',
          (resultKind(result) === 'image' || resultKind(result) === 'video') &&
            'result-preview-media'
        ]"
        @click.self="isMediaPreview && close(false)"
        ><img
          v-if="resultKind(result) === 'image'"
          :src="result.url!"
          :alt="t('previewDialog.resultAlt', { title, n: index + 1 })"
        /><video
          v-else-if="resultKind(result) === 'video'"
          ref="previewVideo"
          controls
          controlsList="nodownload"
          autoplay
          playsinline
          :src="result.url!"
          @loadedmetadata="handleVideoMetadata"
        />
        <pre v-else>{{ result.text || result.url || t('previewDialog.noText') }}</pre>
        <footer v-if="!isMediaPreview && (result.url || result.text)" class="result-preview-footer">
          <span>{{ t('previewDialog.resultLabel', { n: index + 1 }) }}</span>
          <Button
            variant="outline"
            size="sm"
            type="button"
            :disabled="downloadingIndex === index"
            @click="emit('download', result, index)"
            ><LoaderCircle v-if="downloadingIndex === index" class="size-4 animate-spin" /><Download
              v-else
              class="size-4"
            />
            {{ t('previewDialog.download') }}</Button
          >
        </footer></Card
      >
    </div>
  </Dialog>
</template>

<style scoped>
.result-preview-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 20rem), 1fr));
  gap: 0.75rem;
  overflow-y: auto;
}
.result-preview-single {
  background: transparent;
}
.result-preview-single-card {
  position: relative;
  border: 0;
  background: transparent;
}
:deep(.result-preview-media-body) {
  padding: 0;
}
.result-preview-toolbar {
  position: fixed;
  top: 1rem;
  right: 1rem;
  z-index: 2;
  display: flex;
  align-items: center;
  gap: 0.375rem;
  pointer-events: none;
}
.result-preview-toolbar :deep(button) {
  border: 1px solid color-mix(in srgb, white 36%, transparent);
  background: color-mix(in srgb, black 48%, transparent);
  color: white;
  pointer-events: auto;
}
.result-preview-count {
  position: fixed;
  top: 1rem;
  left: 50%;
  z-index: 2;
  transform: translateX(-50%);
  min-width: 3.5rem;
  padding: 0.5rem 0.625rem;
  border: 1px solid color-mix(in srgb, white 36%, transparent);
  background: color-mix(in srgb, black 48%, transparent);
  color: white;
  text-align: center;
  font-size: 0.75rem;
  font-variant-numeric: tabular-nums;
  pointer-events: none;
}
.result-preview-nav {
  position: fixed;
  top: 50%;
  z-index: 2;
  transform: translateY(-50%);
  border-color: color-mix(in srgb, white 36%, transparent);
  background: color-mix(in srgb, black 36%, transparent);
  color: white;
}
.result-preview-nav-previous {
  left: 1rem;
}
.result-preview-nav-next {
  right: 1rem;
}
.result-preview-media {
  display: grid;
  place-items: center;
}
.result-preview-card img,
.result-preview-card video {
  display: block;
  max-width: 100%;
  max-height: 88vh;
  width: auto;
  object-fit: contain;
}
.result-preview-card pre {
  min-height: 8rem;
  overflow: auto;
  padding: 0.75rem;
  white-space: pre-wrap;
  font: inherit;
}
.result-preview-footer {
  display: flex;
  width: 100%;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  border-top: 1px solid var(--border);
  padding: 0.625rem 0.75rem;
  font-size: 0.75rem;
  color: var(--muted-foreground);
}
@media (max-width: 767px) {
  .result-preview-toolbar {
    top: 0.75rem;
    right: 0.75rem;
  }
  .result-preview-count {
    top: 0.75rem;
  }
  .result-preview-nav-previous {
    left: 0.75rem;
  }
  .result-preview-nav-next {
    right: 0.75rem;
  }
  .result-preview-dialog {
    width: 100% !important;
    max-width: none;
    max-height: none;
    height: 100dvh;
  }
  :deep(.result-preview-body) {
    display: flex;
    flex: 1;
    min-height: 0;
    padding: 0;
  }
  .result-preview-grid {
    flex: 1;
    grid-template-columns: 1fr;
    gap: 0;
  }
  .result-preview-card {
    min-height: calc(100dvh - 3.5rem);
    border-right: 0;
    border-left: 0;
  }
  .result-preview-card img,
  .result-preview-card video {
    max-height: calc(100dvh - 7.5rem);
  }
  .result-preview-footer {
    padding-right: 1rem;
    padding-left: 1rem;
  }
}
</style>
