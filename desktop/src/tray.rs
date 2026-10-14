//! 托盘：这个应用的**常驻形式**。
//!
//! 为什么需要它：这个产品的活儿是**分钟级**的云端出图，用户点了「开始生成」必然切走。
//! 没有托盘的话，关窗就等于全停（外壳拥有它拉起的 daemon，见 `main.rs`），
//! 于是"任务在后台跑、跑完告诉你"这条承诺是空的。有了托盘，"关窗"变成"收进托盘"——
//! 外壳与 daemon 都留着，任务照旧被对账与轮询看着，完成时弹系统通知。
//!
//! ⚠️ 托盘图标**本身不是提醒渠道**：Windows 11 默认把托盘图标收进溢出区，用户根本看不到
//! 它在闪。真正会跳到眼前的是完成通知（见 `status_poller.rs`）；托盘是那个"我还在"的锚点，
//! 以及**唯一的退出入口**（关窗不再等于退出）。

use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub const TRAY_ID: &str = "renderloft-tray";

/// 建托盘。返回 `Err` 时调用方**必须**退回"关窗即退出"——见 `main.rs` 里的理由。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开控制台", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出（停止后台 daemon）", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .tooltip("RenderLoft")
        .menu(&menu)
        // 左键点图标 = 打开窗口（Windows 的习惯用法），菜单走右键。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 把主窗口显出来并给焦点。三个入口都走它：托盘左键、菜单「打开控制台」、第二次启动。
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 更新悬停提示。失败就算了——提示是锦上添花，不该把状态轮询带崩。
pub fn set_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(text));
    }
}
