<template>
  <header class="page-header glass-hairline-b">
    <div class="page-heading">
      <p v-if="eyebrow" class="glass-eyebrow glass-eyebrow-lead">{{ eyebrow }}</p>
      <h1 class="page-title">{{ title }}</h1>
      <p v-if="description" class="page-description">{{ description }}</p>
    </div>
    <div v-if="$slots.actions" class="page-actions">
      <slot name="actions" />
    </div>
  </header>
</template>

<script setup lang="ts">
interface Props {
  eyebrow?: string
  title: string
  description?: string
}

defineProps<Props>()
</script>

<style scoped>
.page-header {
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  gap: 1.25rem;
  padding-bottom: 1.25rem;
}
/* 底边的渐隐发丝线由 .glass-hairline-b 提供（见 style.css 的共享类） */
.page-title {
  margin-top: 0.5rem;
  font-family: var(--font-display);
  /* 全站最大的一处字号：标题与正文的尺度差是"层级感"的主要来源，别压回 1.5rem */
  font-size: clamp(1.75rem, 1.35rem + 1.4vw, 2.5rem);
  font-weight: 600;
  line-height: 1.14;
  letter-spacing: 0.01em;
}
.page-description {
  margin-top: 0.75rem;
  max-width: 46rem;
  font-size: 0.9375rem;
  line-height: 1.65;
  color: var(--app-muted-foreground);
}
.page-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}
/* 标题块可以收缩，但**不许缩到读不出字**（见下面 row 模式里的 flex-basis）。 */
.page-heading {
  min-width: 0;
}
@media (min-width: 640px) {
  .page-header {
    flex-direction: row;
    align-items: flex-end;
    /*
     * ⚠️ 必须允许换行。行模式下动作区是 `flex: none`（它不该被压扁），而标题块 `min-width: 0`
     * 可以一直缩到 min-content —— 于是"主列比动作区宽不了多少"时，标题会被压成**一列一个字**
     * （实测：主列 ~500px、三个按钮 471px ⇒ 「应用库」竖着排）。这里给它一个 flex-basis：
     * 放不下就**把动作区换到下一行**，而不是牺牲可读性。
     */
    flex-wrap: wrap;
  }
  .page-heading {
    flex: 1 1 20rem;
  }
  .page-actions {
    flex: none;
  }
}
</style>
