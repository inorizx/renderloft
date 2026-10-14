use serde::{Deserialize, Serialize};

/// 回调签名密钥在系统凭据库里的槽位名。
///
/// ⚠️ 只有**人**能写它（`renderloft-cli callback set-secret --from-file …`）：
/// MCP 那侧没有对应工具（动词表里 `mcp_name: None`），因为 MCP 的参数是模型生成的，
/// 密钥进去就等于进了模型上下文与对话记录。这与"写密钥不给 AI"是同一条边界。
pub const SIGNING_KEY_SLOT: &str = "callback-signing-key";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackSubscription {
    pub url: String,
    pub events: Vec<String>,
    pub secret_ref: Option<String>,
    pub max_attempts: u32,
    pub retry_after_seconds: u64,
}

impl CallbackSubscription {
    pub fn new(url: impl Into<String>, events: Vec<String>) -> Self {
        Self {
            url: url.into(),
            events,
            secret_ref: None,
            max_attempts: 10,
            retry_after_seconds: 30,
        }
    }

    pub fn covers(&self, event_type: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|event| event == event_type)
    }
}

/// 回调地址放行规则：**HTTPS 一律可以，http 只允许回环**。
///
/// 为什么放行回环的 http：本地接收端（自己的小服务、ntfy、n8n…）天然就是
/// `http://127.0.0.1:xxxx`，逼它上 https 等于逼用户自己造证书，实际结果只会是
/// "这个功能没人用"。回环地址不会把快照发到机器外面去，风险与收益是清楚的。
///
/// ⚠️ 只认 `127.0.0.1` / `localhost` / `[::1]`：`127.0.0.0/8` 的其它地址与内网地址
/// （10.x / 192.168.x）**不算回环**——那是"发到局域网里"，不是"发给自己"。
///
/// 放在 domain 里是因为**两处都要用同一条规则**：HTTP 入站校验（准不准建这条订阅）
/// 与出站投递（要不要绕开代理直连）。各写一份迟早会漂移。
pub fn is_acceptable_url(url: &str) -> bool {
    let Some((secure, rest)) = split_scheme(url) else {
        return false;
    };
    let host = host_of(rest);
    if host.is_empty() {
        return false;
    }
    secure || is_loopback_host(host)
}

fn split_scheme(url: &str) -> Option<(bool, &str)> {
    if let Some(rest) = url.strip_prefix("https://") {
        return Some((true, rest));
    }
    url.strip_prefix("http://").map(|rest| (false, rest))
}

/// 从"协议之后的部分"里取出主机名（丢掉端口与路径）；IPv6 字面量去掉方括号。
fn host_of(rest: &str) -> &str {
    if let Some(after_bracket) = rest.strip_prefix('[') {
        // IPv6 字面量：http://[::1]:8080/…
        after_bracket.split(']').next().unwrap_or_default()
    } else {
        rest.split([':', '/']).next().unwrap_or_default()
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_covers_every_event() {
        let subscription = CallbackSubscription::new("https://example.com/cb", vec![]);
        assert!(subscription.covers("operation.terminal"));
        assert!(subscription.covers("run.updated"));
    }

    #[test]
    fn explicit_filter_restricts_events() {
        let subscription = CallbackSubscription::new(
            "https://example.com/cb",
            vec!["operation.terminal".to_string()],
        );
        assert!(subscription.covers("operation.terminal"));
        assert!(!subscription.covers("run.updated"));
    }

    /// 放行规则：https 一律可以，http 只允许回环。
    ///
    /// 负对照：把 `is_acceptable_url` 改回"只认 https://" ⇒ 回环那几条红；
    /// 改成"http 一律放行" ⇒ 内网/公网那几条红。两边都要钉住，否则口子会越开越大。
    #[test]
    fn callback_urls_allow_https_and_loopback_http_only() {
        for allowed in [
            "https://example.com/hook",
            "https://example.com:8443/hook?x=1",
            "http://127.0.0.1:8787/hook",
            "http://127.0.0.1/hook",
            "http://localhost:3000/",
            "http://LOCALHOST:3000/", // 主机名大小写不敏感
            "http://[::1]:9000/hook",
        ] {
            assert!(is_acceptable_url(allowed), "应当放行：{allowed}");
        }
        for rejected in [
            "",
            "http://example.com/hook",    // 明文发到公网
            "http://10.0.0.5:8080/hook",  // 内网 ≠ 回环
            "http://192.168.1.10/hook",   // 同上
            "http://127.0.0.2:8080/hook", // 127/8 的其它地址也不放行（只放行 127.0.0.1）
            "ftp://127.0.0.1/hook",       // 协议不对
            "https://",                   // 只有协议头，没有主机
            "not a url",
        ] {
            assert!(!is_acceptable_url(rejected), "应当拒绝：{rejected}");
        }
    }

    /// 回环判定与放行规则用的是同一套主机名解析（只认 `127.0.0.1` / `localhost` / `[::1]`）。
    ///
    /// 钉住"内网不算回环"这件事：放行规则与将来任何"按回环特判"的逻辑都靠它。
    #[test]
    fn loopback_hosts_are_exactly_the_three_we_allow() {
        for loopback in [
            "http://127.0.0.1:8787/hook",
            "http://localhost/hook",
            "http://[::1]:9000/hook",
        ] {
            let (_, rest) = split_scheme(loopback).expect("应当认得协议头");
            assert!(is_loopback_host(host_of(rest)), "应当算回环：{loopback}");
        }
        for remote in [
            "https://example.com/hook",
            "http://10.0.0.5/hook",
            "http://127.0.0.2/hook",
        ] {
            let (_, rest) = split_scheme(remote).expect("应当认得协议头");
            assert!(!is_loopback_host(host_of(rest)), "不该算回环：{remote}");
        }
    }
}
