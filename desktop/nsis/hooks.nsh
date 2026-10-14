; RenderLoft 安装钩子：把 CLI 加进**用户** PATH。
;
; 为什么需要：安装包把 `renderloft-cli.exe` 放进安装目录，但不会自动进 PATH ——
; 用户装了却敲不出命令，会以为"CLI 要单独装"。实测（2026-10-09）：Python 安装器那个
; "Add to PATH" 默认不勾的坑就是这么来的，所以这里**默认加**，不给勾选框。
;
; 只动 HKCU（用户级）⇒ 不需要管理员，也不需要 UAC；卸载时**必须**删掉，
; 否则 PATH 里会留一条指向已删除目录的死路径。
;
; ⚠️ 摘除一律用**带分号的锚定形态**（`$INSTDIR;` 与 `;$INSTDIR`），不做裸子串替换：
; PATH 里若有邻居路径 `C:\X\RenderLoft2`，裸删 `C:\X\RenderLoft` 会把它改成 `2`。
; 只有"整串恰好等于 $INSTDIR"时才清空。
;
; ⚠️ StrFunc 的三个坑（都实测踩过）：
;   ① 声明写 `${Using:StrFunc} StrRep`，**不是** `${StrFunc} StrRep`；
;   ② 卸载器要**另外**声明 `${Using:StrFunc} UnStrRep`，且卸载器里只能调 `${UnStrRep}`
;      （`StrRep` 那份函数只存在于安装器里，混用会编译失败：
;        "You forgot ${Using:StrFunc} UnStrRep"）；
;   ③ **不能**把 `${StrRep}` 当宏参数传——它展开后含空格，会被当成 3 个参数
;      （实测：`macro requires 1 parameter(s), passed 3`）；`!define` 也吃不下它。
;      所以 `RENDERLOFT_STRIP_PATH` 收的是一个单词（`inst`/`un`），内部用 `!if` 选函数。
;
; 这段逻辑单独测过：`node scripts/test-nsis-path-hook.mjs`
; —— 用临时注册表键当假 PATH，把**真文件** include 进去，直接调下面两个钩子跑边界用例
;    （含"邻居路径不能被改坏""重复安装/卸载不能堆第二条""值不存在时绝不创建空 PATH"），
;    跑完不碰真实 PATH。
;
; Tauri 支持的钩子（见 tauri CLI 的 config.schema.json）：
;   NSIS_HOOK_PREINSTALL / POSTINSTALL / PREUNINSTALL / POSTUNINSTALL

!include "LogicLib.nsh"
!include "WinMessages.nsh"
!include "StrFunc.nsh"

${Using:StrFunc} StrRep
${Using:StrFunc} UnStrRep

; 注册表位置做成可覆盖：测试用临时键替换掉它，就不必碰真实 PATH。
!ifndef RENDERLOFT_ENV_KEY
  !define RENDERLOFT_ENV_KEY "Environment"
!endif
!ifndef RENDERLOFT_ENV_VALUE
  !define RENDERLOFT_ENV_VALUE "Path"
!endif

; 把 $INSTDIR 从 PATH 里摘掉（幂等：不在里面就原样返回）。
; 入参是**一个单词**：`inst`（安装器）或 `un`（卸载器）——见文件头坑③，函数名不能当参数传，
; 所以这里用 !if 在编译期选（没被选中的那一支不会被展开，因此不会报"未声明"）。
; 出参：$0 = 处理后的 PATH，$1 = 1 表示注册表里本来就有这个值（0 = 没有）。
!macro RENDERLOFT_STRIP_PATH _which
  StrCpy $1 0
  ClearErrors
  ReadRegStr $0 HKCU "${RENDERLOFT_ENV_KEY}" "${RENDERLOFT_ENV_VALUE}"
  ${IfNot} ${Errors}
    StrCpy $1 1
    !if "${_which}" == "un"
      ${UnStrRep} $0 $0 "$INSTDIR;" ""
      ${UnStrRep} $0 $0 ";$INSTDIR" ""
    !else
      ${StrRep} $0 $0 "$INSTDIR;" ""
      ${StrRep} $0 $0 ";$INSTDIR" ""
    !endif
    ${If} $0 == "$INSTDIR"
      StrCpy $0 ""
    ${EndIf}
  ${EndIf}
!macroend

; 通知已开着的程序环境变了。新开的进程本来就会读到新 PATH，这一步是为了资源管理器
; 这类**已经跑着**的进程（不改也不影响命令行，属于礼貌）。
; 测试里用 `RENDERLOFT_SKIP_BROADCAST` 跳过：广播要等窗口响应，会让测试白等几十秒。
!macro RENDERLOFT_BROADCAST_ENV
  !ifndef RENDERLOFT_SKIP_BROADCAST
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=3000
  !endif
!macroend

; 装完：摘干净再追加（所以重复安装/升级不会堆出第二条）。
!macro NSIS_HOOK_POSTINSTALL
  !insertmacro RENDERLOFT_STRIP_PATH inst
  ${If} $0 == ""
    StrCpy $0 "$INSTDIR"
  ${Else}
    StrCpy $0 "$0;$INSTDIR"
  ${EndIf}
  WriteRegExpandStr HKCU "${RENDERLOFT_ENV_KEY}" "${RENDERLOFT_ENV_VALUE}" "$0"
  !insertmacro RENDERLOFT_BROADCAST_ENV
!macroend

; 卸载前：摘掉自己那一段。注册表里本来没这个值就什么都不写——绝不制造一个空 PATH。
!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro RENDERLOFT_STRIP_PATH un
  ${If} $1 == 1
    WriteRegExpandStr HKCU "${RENDERLOFT_ENV_KEY}" "${RENDERLOFT_ENV_VALUE}" "$0"
    !insertmacro RENDERLOFT_BROADCAST_ENV
  ${EndIf}
!macroend
