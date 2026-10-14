import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'
import { readFileSync } from 'node:fs'

/**
 * daemon 端点文件（内含 Bearer token）。
 *
 * 目录名必须与 daemon 侧**同源**：所有者是 `src-tauri/src/v2/daemon.rs` 的 `v2_data_dir()`，
 * Rust 客户端 `v2/clients/client.rs` 与桌面外壳 `desktop/src/main.rs` 读的是同一路径。
 * 改目录名时这几处必须一起改——`scripts/check-layers.mjs` 会替你检查。
 *
 * ⚠️ 不一致的后果很难看出病因：代理**永远读不到 token**，每个请求都以未认证发出，
 * daemon 回 401「缺少 Bearer 认证令牌」——看起来像"鉴权坏了"，实际只是两个手写字符串不一致。
 */
const daemonEndpointPath = resolve(
  process.env.LOCALAPPDATA || '.',
  'renderloft-workspace',
  'daemon-endpoint.json'
)

/**
 * 已读到的 token 缓存。
 *
 * ⚠️ 为什么需要它：原先每个请求都 `readFileSync`，而**读失败会抛异常**——`proxyReq`
 * 处理器一抛异常就再也不会执行 `setHeader`，请求于是带着**空凭据**发出去，daemon 回
 * 「缺少 Bearer 认证令牌」。看起来像鉴权坏了，实际只是端点文件暂时读不到
 * （`pnpm dev` 启动期间 daemon 还在编译、文件尚未写出，或正被重写）。
 *
 * 缓存 + 读失败回退到旧值，就把这类瞬时故障挡掉了；真的一个 token 都没有时，
 * 至少把原因**明确打出来**，而不是让开发者对着一个误导性的鉴权错误猜。
 */
let cachedToken: string | null = null

const daemonCredentials = (): string | null => {
  try {
    const parsed = JSON.parse(readFileSync(daemonEndpointPath, 'utf8')) as { token: string }
    cachedToken = parsed.token
    return parsed.token
  } catch (error) {
    if (cachedToken) return cachedToken
    console.warn(
      `[vite] 读不到 daemon 端点文件 ${daemonEndpointPath}（${error instanceof Error ? error.message : String(error)}）；` +
        'daemon 可能仍在编译或尚未启动。该请求将以未认证发出，稍后会自动恢复。'
    )
    return null
  }
}

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src')
    }
  },
  server: {
    host: '127.0.0.1',
    port: 5174,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:31337',
        changeOrigin: false,
        configure: (proxy) =>
          proxy.on('proxyReq', (request) => {
            const token = daemonCredentials()
            if (token) request.setHeader('authorization', `Bearer ${token}`)
          })
      },
      '/events': {
        target: 'http://127.0.0.1:31337',
        changeOrigin: false,
        configure: (proxy) =>
          proxy.on('proxyReq', (request) => {
            const token = daemonCredentials()
            if (token) request.setHeader('authorization', `Bearer ${token}`)
          })
      }
    }
  }
})
