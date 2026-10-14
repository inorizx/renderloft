import { spawnSync } from 'node:child_process'

const run = (command, args, cwd = process.cwd()) => {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' })
  if (result.status !== 0) process.exit(result.status ?? 1)
}

run('pnpm', ['build'])
run('cargo', ['build', '--release', '--bin', 'renderloft', '--bin', 'renderloft-cli', '--bin', 'renderloft-mcp'], 'src-tauri')
run('pnpm', ['exec', 'tauri', 'build', '--config', 'desktop/tauri.conf.json'])
