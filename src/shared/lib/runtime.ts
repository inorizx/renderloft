// 运行时环境探测。语义上不属于 api 层，归置到 lib。

export const isMockRuntime = () =>
  import.meta.env.DEV && new URLSearchParams(window.location.search).get('mock') === '1'
export const isDesktopRuntime = () =>
  typeof window !== 'undefined' &&
  Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__)
