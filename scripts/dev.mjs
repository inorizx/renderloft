import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import path from 'node:path'

const daemonHealthy = async () => {
  try {
    return (await fetch('http://127.0.0.1:31337/health')).ok
  } catch {
    return false
  }
}

// 前端 dev server 端口：与 vite.config.ts 的 server.port 必须一致。
// 5174 是本项目的专属端口，避免与本机其他 vite 项目（默认 5173）冲突。
const UI_ORIGIN = 'http://127.0.0.1:5174'

const children = []
if (!(await daemonHealthy())) {
  const daemonPath = path.join('src-tauri', 'target', 'debug', process.platform === 'win32' ? 'renderloft.exe' : 'renderloft')
  const rebuildDaemon = process.env.RENDERLOFT_DEV_REBUILD === '1' || !existsSync(daemonPath)
  const daemon = rebuildDaemon
    ? ['cargo', ['run', '--manifest-path', 'src-tauri/Cargo.toml', '--bin', 'renderloft']]
    : [daemonPath, []]
  children.push(spawn(daemon[0], daemon[1], {
    stdio: 'inherit',
    env: { ...process.env, RENDERLOFT_UI_ORIGIN: UI_ORIGIN },
  }))
}

const vite = process.platform === 'win32'
  ? spawn(process.env.ComSpec ?? 'cmd.exe', ['/d', '/s', '/c', 'pnpm vite --strictPort'], { stdio: 'inherit' })
  : spawn('pnpm', ['vite', '--strictPort'], { stdio: 'inherit' })
children.push(vite)

const stop = () => children.forEach((child) => child.kill())
process.on('SIGINT', stop)
process.on('SIGTERM', stop)
children.forEach((child) => child.on('exit', (code) => { if (code && code !== 0) stop() }))
