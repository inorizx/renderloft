import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import path from 'node:path'

// macOS build. Must run ON a macOS host (Tauri cannot cross-compile macOS apps from Windows).
const root = process.cwd()
const run = (command, args, cwd = root) => {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
  if (result.status !== 0) process.exit(result.status ?? 1)
}

run('pnpm', ['build'])
run('cargo', ['build', '--release', '--bin', 'renderloft', '--bin', 'renderloft-cli', '--bin', 'renderloft-mcp'], 'src-tauri')

// Tauri always merges --config into desktop/tauri.conf.json, so temporarily swap in a full
// macOS config. This avoids the Windows-only .exe resources leaking into the .app bundle.
const configPath = path.join(root, 'desktop', 'tauri.conf.json')
const backupPath = path.join(root, 'desktop', 'tauri.conf.json.windows.bak')
const macosConfig = JSON.parse(readFileSync(configPath, 'utf8'))
macosConfig.bundle = {
  active: true,
  targets: ['app', 'dmg'],
  icon: ['../src-tauri/icons/icon.icns'],
  resources: {
    '../src-tauri/target/release/renderloft': 'renderloft',
    '../src-tauri/target/release/renderloft-cli': 'renderloft-cli',
    '../src-tauri/target/release/renderloft-mcp': 'renderloft-mcp'
  }
}
try {
  renameSync(configPath, backupPath)
  writeFileSync(configPath, JSON.stringify(macosConfig, null, 2))
  run('pnpm', ['exec', 'tauri', 'build'])
} finally {
  if (existsSync(backupPath)) {
    renameSync(backupPath, configPath)
  }
}
