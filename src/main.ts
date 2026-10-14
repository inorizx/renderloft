import { createApp } from 'vue'
import { createPinia } from 'pinia'
import router from './router'
import './style.css'
import App from './App.vue'

/*
 * 全局兜底：**别让一次"落空"的拖放把整个界面导航走**。
 *
 * 背景（2026-10-13）：桌面外壳里 `dragDropEnabled: false`（见 `desktop/tauri.conf.json`），
 * 这样网页层才收得到文件拖放（Tauri 默认会注册它自己的 drop handler 并把 WebView2 的
 * `AllowExternalDrop` 关掉，于是 `@drop` 永远不触发——运行页那句"或拖拽文件到此处"
 * 在桌面端一直是坏的）。换来的是浏览器的默认行为也回来了：把文件拖到**没有** drop 处理的
 * 区域上，Chromium 会**导航到那个文件**（整个应用被换成一个 JSON/图片）。
 *
 * 这里在 window 上兜住默认行为。组件里的 `@drop.prevent` 先跑（target 阶段 → 冒泡），
 * 这个监听只负责收拾"没人接"的那些，不影响任何已有的拖放区。
 */
for (const type of ['dragover', 'drop'] as const) {
  window.addEventListener(type, (event) => event.preventDefault())
}

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.mount('#app')
