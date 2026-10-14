<script setup lang="ts">
import { cn } from '@/lib/utils'

interface Props {
  /**
   * 输入框的值。允许 `number`：调用方常直接绑数字字段（温度、轮数、超时秒数），
   * 而 `<input>` 的 value 本来就是字符串——**回传的永远是字符串**，取用时自行转换
   * （Agent 表单在提交时用 `Number(...)` 收口）。
   *
   * 只声明 `string` 会让每次打开那个表单都刷 3 条 Vue 类型告警，而行为其实是对的。
   */
  modelValue?: string | number
  id?: string
  type?: string
  placeholder?: string
  class?: string
}

const props = defineProps<Props>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()

const handleInput = (event: Event) => {
  emit('update:modelValue', (event.target as HTMLInputElement).value)
}
</script>

<template>
  <input
    :id="id"
    :type="type || 'text'"
    :value="modelValue"
    :placeholder="placeholder"
    :class="
      cn(
        'flex h-9 w-full rounded-md border border-input bg-secondary px-3 text-sm shadow-xs outline-none transition-[border-color,box-shadow] duration-150 placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/20 disabled:cursor-not-allowed disabled:opacity-50',
        props.class
      )
    "
    @input="handleInput"
  />
</template>
