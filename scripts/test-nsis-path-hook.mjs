// NSIS PATH 钩子的边界测试（`desktop/nsis/hooks.nsh`）。
//
// 为什么值得单独写：这段逻辑会改**每个用户**的 PATH，而它的失败模式很脏——
// 把邻居路径改坏（`C:\X\RenderLoft2` 被截成 `2`）、重复安装堆出第二条、
// 卸载后留一条死路径。这些都不是"装一次试试"能覆盖的。
//
// 做法：把**真文件** include 进一个临时 .nsi，但把注册表位置 `RENDERLOFT_ENV_KEY`
// 换成临时键 ⇒ 跑的是要发货的那份逻辑，而**不碰真实 PATH**。
//
// 需要 makensis（Tauri 打包时会下载到 %LOCALAPPDATA%\tauri\NSIS）。
// 用法：node scripts/test-nsis-path-hook.mjs
import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const ROOT = join(import.meta.dirname, '..')
const HOOKS = join(ROOT, 'desktop', 'nsis', 'hooks.nsh')
const SCRATCH_KEY = 'Software\\RenderLoftPathHookTest'
const FAKE_INSTDIR = 'C:\\X\\RenderLoft'

const makensis = join(
  process.env.LOCALAPPDATA ?? '',
  'tauri',
  'NSIS',
  'makensis.exe',
)
if (!existsSync(makensis)) {
  console.error(`找不到 makensis：${makensis}`)
  console.error('先跑一次 `pnpm build:windows`（Tauri 会把 NSIS 下载到这里），再运行本脚本。')
  process.exit(1)
}
if (!existsSync(HOOKS)) {
  console.error(`找不到钩子文件：${HOOKS}`)
  process.exit(1)
}

const work = mkdtempSync(join(tmpdir(), 'renderloft-path-hook-'))
const resultInstall = join(work, 'result-install.txt')
const resultUninstall = join(work, 'result-uninstall.txt')
const installer = join(work, 'path-hook-test.exe')

// 把钩子**原样复制**到 ASCII 临时目录再 include：
// makensis 是 ANSI 程序，`!include` 指向**含非 ASCII 字符的路径**时它读不到
// （实测报 `could not find`）。复制的是同一份字节，测的仍然是要发货的逻辑。
const hooksCopy = join(work, 'hooks.nsh')
writeFileSync(hooksCopy, readFileSync(HOOKS))

// 用例：初始 PATH → 装完（POSTINSTALL 后）→ 卸完（POSTUNINSTALL 后）。
// 第 6、7 条是**负对照的关键**：邻居路径 `C:\X\RenderLoft2` 一个字符都不能变。
const CASES = [
  ['C:\\A;C:\\X\\RenderLoft;C:\\B', 'C:\\A;C:\\B;C:\\X\\RenderLoft', 'C:\\A;C:\\B'],
  ['C:\\X\\RenderLoft;C:\\B', 'C:\\B;C:\\X\\RenderLoft', 'C:\\B'],
  ['C:\\A;C:\\X\\RenderLoft', 'C:\\A;C:\\X\\RenderLoft', 'C:\\A'],
  ['C:\\X\\RenderLoft', 'C:\\X\\RenderLoft', ''],
  ['C:\\A', 'C:\\A;C:\\X\\RenderLoft', 'C:\\A'],
  ['C:\\X\\RenderLoft2;C:\\A', 'C:\\X\\RenderLoft2;C:\\A;C:\\X\\RenderLoft', 'C:\\X\\RenderLoft2;C:\\A'],
  ['C:\\X\\RenderLoft;C:\\X\\RenderLoft2', 'C:\\X\\RenderLoft2;C:\\X\\RenderLoft', 'C:\\X\\RenderLoft2'],
]

// 安装路径与卸载路径**必须在各自的段里测**：StrFunc 会拒绝在普通段里调用 `UnStrRep`
// （"Call must not be used with functions starting with un. in the non-uninstall sections"），
// 所以下面真的生成一个卸载器，跑完安装器再跑卸载器。
const installCases = CASES.map(
  ([initial, afterInstall], index) => `
  ; 用例 ${index + 1}：初始 [${initial}]
  WriteRegExpandStr HKCU "${SCRATCH_KEY}" "Path" "${initial}"
  StrCpy $INSTDIR "${FAKE_INSTDIR}"
  !insertmacro NSIS_HOOK_POSTINSTALL
  !insertmacro ASSERT "$0" "${afterInstall}" "case${index + 1}-install"
  ; 幂等：重复安装不该堆出第二条
  !insertmacro NSIS_HOOK_POSTINSTALL
  !insertmacro NSIS_HOOK_POSTINSTALL
  !insertmacro ASSERT "$0" "${afterInstall}" "case${index + 1}-install-idempotent"`,
).join('\n')

const uninstallCases = CASES.map(
  // 卸载用例只关心"装完的 PATH"与"卸完应该是什么"，第一个元素（初始 PATH）用不上。
  ([, afterInstall, afterUninstall], index) => `
  ; 用例 ${index + 1}：装完是 [${afterInstall}]
  WriteRegExpandStr HKCU "${SCRATCH_KEY}" "Path" "${afterInstall}"
  StrCpy $INSTDIR "${FAKE_INSTDIR}"
  !insertmacro NSIS_HOOK_POSTUNINSTALL
  !insertmacro ASSERT "$0" "${afterUninstall}" "case${index + 1}-uninstall"
  ; 幂等：重复卸载不该再变
  !insertmacro NSIS_HOOK_POSTUNINSTALL
  !insertmacro NSIS_HOOK_POSTUNINSTALL
  !insertmacro ASSERT "$0" "${afterUninstall}" "case${index + 1}-uninstall-idempotent"`,
).join('\n')

const script = `
!define RENDERLOFT_ENV_KEY "${SCRATCH_KEY}"
!define RENDERLOFT_ENV_VALUE "Path"
!define RENDERLOFT_SKIP_BROADCAST
!include "${hooksCopy}"

Name "RenderLoft path hook test"
OutFile "${installer}"
InstallDir "${join(work, 'installed')}"
SilentInstall silent
RequestExecutionLevel user

!macro ASSERT _actual _expected _label
  \${If} "\${_actual}" == "\${_expected}"
    FileWrite $9 "PASS \${_label}$\\r$\\n"
  \${Else}
    FileWrite $9 "FAIL \${_label} got=[\${_actual}] want=[\${_expected}]$\\r$\\n"
  \${EndIf}
!macroend

Section
  ; WriteUninstaller 要求目录已存在（真实安装器靠 SetOutPath 建）
  CreateDirectory "$INSTDIR"
  SetOutPath "$INSTDIR"
  ; 用例会把 $INSTDIR 换成假路径，先备份真的（否则 WriteUninstaller 会写到假目录去）
  StrCpy $4 $INSTDIR
  DeleteRegKey HKCU "${SCRATCH_KEY}"
  FileOpen $9 "${resultInstall}" w
${installCases}

  ; 值不存在时：安装钩子应当把它建出来，内容就是 $INSTDIR
  DeleteRegKey HKCU "${SCRATCH_KEY}"
  StrCpy $INSTDIR "${FAKE_INSTDIR}"
  !insertmacro NSIS_HOOK_POSTINSTALL
  !insertmacro ASSERT "$0" "${FAKE_INSTDIR}" "missing-value-install-creates"
  FileClose $9

  StrCpy $INSTDIR $4
  WriteUninstaller "$INSTDIR\\uninst.exe"
SectionEnd

Section "Uninstall"
  ; 用例要把 $INSTDIR 换成假路径，先备份真的，结束时还原（否则卸载器会去删假目录）
  StrCpy $3 $INSTDIR
  FileOpen $9 "${resultUninstall}" w
${uninstallCases}

  ; 值不存在时：卸载钩子**绝不能**创建一个空 PATH（那等于把用户的 PATH 清掉）
  DeleteRegKey HKCU "${SCRATCH_KEY}"
  StrCpy $INSTDIR "${FAKE_INSTDIR}"
  !insertmacro NSIS_HOOK_POSTUNINSTALL
  ClearErrors
  ReadRegStr $2 HKCU "${SCRATCH_KEY}" "Path"
  \${If} \${Errors}
    FileWrite $9 "PASS missing-value-still-missing$\\r$\\n"
  \${Else}
    FileWrite $9 "FAIL missing-value-created got=[$2]$\\r$\\n"
  \${EndIf}
  FileClose $9
  StrCpy $INSTDIR $3
SectionEnd
`

const nsiPath = join(work, 'test.nsi')
writeFileSync(nsiPath, script)

console.log(`makensis: ${makensis}`)
// /INPUTCHARSET UTF8：钩子文件里是中文注释，按 ANSI 读会乱码甚至解析失败。
const compiled = spawnSync(makensis, ['/V2', '/INPUTCHARSET', 'UTF8', nsiPath], {
  encoding: 'utf8',
})
if (compiled.status !== 0) {
  console.error('编译失败：')
  console.error(compiled.stdout ?? '')
  console.error(compiled.stderr ?? '')
  process.exit(1)
}

const ran = spawnSync(installer, ['/S'], { encoding: 'utf8' })
if (ran.status !== 0) {
  console.error(`安装器退出码 ${ran.status}`)
  process.exit(1)
}
const uninstaller = join(work, 'installed', 'uninst.exe')
if (!existsSync(uninstaller)) {
  console.error(`卸载器没生成：${uninstaller}`)
  process.exit(1)
}
const uninstalled = spawnSync(uninstaller, ['/S'], { encoding: 'utf8' })
if (uninstalled.status !== 0) {
  console.error(`卸载器退出码 ${uninstalled.status}`)
  process.exit(1)
}

// ⚠️ NSIS 卸载器会把自己复制到临时目录再**重新启动**，父进程立刻就返回了——
// 直接读结果文件会 ENOENT（实测踩过）。所以要等文件真的出现。
const sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms)
const waitFor = (file, timeoutMs = 30_000) => {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    // 最后一条断言写完就说明卸载段跑完了
    if (existsSync(file) && readFileSync(file, 'utf8').includes('missing-value')) return true
    sleep(200)
  }
  return false
}
if (!waitFor(resultUninstall)) {
  console.error(`等不到卸载结果：${resultUninstall}`)
  process.exit(1)
}

const lines = [resultInstall, resultUninstall]
  .flatMap((file) => readFileSync(file, 'utf8').split(/\r?\n/))
  .filter((line) => line.trim() !== '')
lines.forEach((line) => console.log('  ' + line))

// 收尾：删掉临时注册表键与临时目录（测试不该留下任何痕迹）
spawnSync('reg', ['delete', `HKCU\\${SCRATCH_KEY}`, '/f'], { stdio: 'ignore' })
rmSync(work, { recursive: true, force: true })

const failures = lines.filter((line) => line.startsWith('FAIL'))
if (failures.length > 0) {
  console.error(`\n${failures.length} 条失败`)
  process.exit(1)
}
console.log(
  `\n全部通过：${lines.length} 条断言（${CASES.length} 个用例 × 安装/卸载 + 2 条"值不存在"）`,
)
