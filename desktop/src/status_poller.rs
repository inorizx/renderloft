//! 后台状态轮询：给托盘提示与"刚跑完"的系统通知提供数据。
//!
//! ⚠️ 铁律一：外壳**不是**业务宿主。所以这里只读 daemon 的 HTTP 面（与 `main.rs` 里换 nonce、
//! 请求关闭走同一条路：本机回环 + 几十字节 JSON），不碰 SQLite，也不自己解释任务语义——
//! "什么是终态"照抄 daemon 给的状态字符串（`SUCCEEDED` / `FAILED` / `CANCELLED`）。
//!
//! ⚠️ 通知**只在"上一轮还在跑、这一轮已经终态"时发**：不这么做的话，每次轮询都会把历史里
//! 每一个终态任务重播一遍（20 条通知刷屏，比不发还糟）。

use crate::tray;
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// 轮询间隔。3 秒是"刚跑完就弹"与"别一直空转"之间的折中：一条 HTTP 请求几十字节。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 一次看多少条。托盘只关心"有没有在跑"和"刚跑完的是哪个"，不需要全量。
const WINDOW: usize = 20;

pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        // 上一轮**还在跑**的任务号：它变成终态时才知道该通知谁。
        let mut running_before: Vec<u64> = Vec::new();
        let mut primed = false;
        loop {
            thread::sleep(POLL_INTERVAL);
            // daemon 没起、或端点文件还没写出来：这一轮跳过，下一轮再说。
            let Some(items) = fetch_operations() else {
                continue;
            };
            let operations: Vec<(u64, String)> = items.iter().filter_map(parse_operation).collect();
            let running_now: Vec<u64> = operations
                .iter()
                .filter(|(_, status)| !is_terminal(status))
                .map(|(id, _)| *id)
                .collect();

            tray::set_tooltip(&app, &tooltip_text(running_now.len()));

            // 第一次只建立基线：否则启动瞬间会把历史里的终态任务全"通知"一遍。
            if primed {
                for (id, status) in &operations {
                    if is_terminal(status) && running_before.contains(id) {
                        notify_finished(&app, *id, status);
                    }
                }
            }
            running_before = running_now;
            primed = true;
        }
    });
}

fn parse_operation(item: &Value) -> Option<(u64, String)> {
    Some((
        item.get("operationId")?.as_u64()?,
        item.get("status")?.as_str()?.to_string(),
    ))
}

fn is_terminal(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "CANCELLED")
}

fn tooltip_text(running: usize) -> String {
    match running {
        0 => "RenderLoft · 空闲（关闭窗口后仍在后台看着任务）".to_string(),
        1 => "RenderLoft · 1 个任务进行中".to_string(),
        n => format!("RenderLoft · {n} 个任务进行中"),
    }
}

fn notify_finished(app: &AppHandle, operation_id: u64, status: &str) {
    let (title, body) = match status {
        "SUCCEEDED" => ("任务完成", format!("任务 #{operation_id} 已完成")),
        "FAILED" => (
            "任务失败",
            format!("任务 #{operation_id} 失败了（详情见任务页）"),
        ),
        "CANCELLED" => ("任务已取消", format!("任务 #{operation_id} 已取消")),
        _ => return,
    };
    // 通知发不出去（系统关了通知、专注助手开着）不该影响任何别的事。
    let _ = app.notification().builder().title(title).body(body).show();
}

fn fetch_operations() -> Option<Vec<Value>> {
    let endpoint = crate::read_endpoint()?;
    let address = endpoint
        .endpoint
        .strip_prefix("http://")
        .unwrap_or(&endpoint.endpoint);
    let body = http_get_json(
        address,
        &format!("/api/v1/operations?limit={WINDOW}"),
        &endpoint.token,
    )?;
    Some(body.get("data")?.get("items")?.as_array()?.clone())
}

/// 手写一次 HTTP/1.1 GET。
///
/// 与 `main.rs` 里那两个请求同一个理由：连接是本机回环、响应是几十字节 JSON，
/// 为它引入一个 HTTP 客户端不划算。
fn http_get_json(address: &str, path: &str, token: &str) -> Option<Value> {
    let mut stream = TcpStream::connect(address).ok()?;
    // 超时必须给：daemon 万一不回，别把轮询线程卡死在一轮里。
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(1500)))
        .ok()?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked(body)?
    } else {
        body.to_string()
    };
    serde_json::from_str(body.trim()).ok()
}

/// 极简 chunked 解码。正常情况下服务端给的是 `Content-Length`，走不到这里；
/// 但"走不到"和"假设不会"是两件事——分块一旦出现，不处理就是静默拿不到状态。
fn decode_chunked(body: &str) -> Option<String> {
    let mut rest = body;
    let mut out = String::new();
    loop {
        let (size_line, tail) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        out.push_str(tail.get(..size)?);
        rest = tail.get(size..)?.strip_prefix("\r\n")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_the_ones_the_daemon_reports() {
        // 判据照抄 daemon（`clients/client.rs::is_terminal_status`）——两边不一致的话，
        // 症状是"任务跑完了但从不通知"或"通知个没完"，都不报错。
        for status in ["SUCCEEDED", "FAILED", "CANCELLED"] {
            assert!(is_terminal(status), "{status} 应当是终态");
        }
        for status in ["QUEUED", "RUNNING", "PARTIAL"] {
            assert!(!is_terminal(status), "{status} 不该被当成终态");
        }
    }

    #[test]
    fn tooltip_says_how_many_are_running() {
        assert!(tooltip_text(0).contains("空闲"));
        assert!(tooltip_text(1).contains("1 个任务"));
        assert!(tooltip_text(7).contains("7 个任务"));
    }

    #[test]
    fn chunked_bodies_are_decoded_instead_of_silently_lost() {
        let body = "7\r\n{\"a\":1}\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body).as_deref(), Some("{\"a\":1}"));
    }
}
