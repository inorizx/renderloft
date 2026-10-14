use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr};
use tokio::net::lookup_host;

use crate::v2::domain::errors::{ErrorKind, SafeError};

pub const MAX_RESULT_BYTES: usize = 100 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 140 * 1024 * 1024;

/// Provider 端点允许的地址范围。
///
/// 默认最严：必须 https，且解析结果不得落在私网。两个放宽条件**都必须显式开**：
///   - `allow_local_http`：本机服务（ComfyUI 的 `127.0.0.1:8188`）允许明文 http；
///   - `allow_private`：局域网地址（显卡机上的 ComfyUI，`192.168.x.x:8188`）允许明文 http
///     —— 由连接的 `config.allowPrivateNetwork = true` 显式开启。
///
/// 为什么默认关：这里是 SSRF 守卫。放开等于允许 daemon 去打内网的任意地址，而连接的
/// URL 是可能被"导入的应用 / 工作流"间接影响的输入。要放开就一条连接一条连接地放。
#[derive(Clone, Copy, Debug)]
pub struct NetworkPolicy {
    pub allow_local_http: bool,
    pub allow_private: bool,
}

impl NetworkPolicy {
    /// 云端 Provider：必须 https，不得私网。
    pub const STRICT: Self = Self {
        allow_local_http: false,
        allow_private: false,
    };
    /// 本机服务：loopback 上允许明文 http。
    pub const LOCAL_HTTP: Self = Self {
        allow_local_http: true,
        allow_private: false,
    };
    /// 本机 + 该连接显式允许的局域网地址。
    pub const LOCAL_HTTP_AND_PRIVATE: Self = Self {
        allow_local_http: true,
        allow_private: true,
    };
}

/// 校验 Provider 端点：仅允许 https，或明确允许的明文 http。
pub fn validate_base_url(raw: &str, policy: NetworkPolicy) -> Result<Url, SafeError> {
    let url = Url::parse(raw)
        .map_err(|_| SafeError::validation("PROVIDER_URL_INVALID", "Provider 地址无效"))?;
    let is_loopback_host = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    match url.scheme() {
        "https" => {}
        "http" if policy.allow_local_http && is_loopback_host => {}
        // 显式允许局域网：明文 http 放行到这里，**是否真的在私网由 validate_resolved_url
        // 按 DNS 结果判定** —— 这样 `http://gpu-box:8188` 这种主机名也能用，而解析到
        // 公网的明文地址仍会被拒。
        "http" if policy.allow_private => {}
        _ => {
            return Err(SafeError::validation(
                "PROVIDER_URL_INSECURE",
                "Provider 地址必须使用 HTTPS（本机 loopback 允许 HTTP；局域网地址需在该连接里显式允许）",
            ))
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SafeError::validation(
            "PROVIDER_URL_CREDENTIALS",
            "Provider 地址不允许携带用户信息",
        ));
    }
    if url.host_str().is_none() {
        return Err(SafeError::validation(
            "PROVIDER_URL_NO_HOST",
            "Provider 地址缺少主机",
        ));
    }
    Ok(url)
}

/// 在 base 上追加路径。base 无尾斜杠时先补齐，避免 `Url::join` 替换最后一段。
pub fn join_path(base: &Url, path: &str) -> Result<Url, SafeError> {
    let mut url = base.clone();
    if !url.path().ends_with('/') {
        let path = url.path().to_string();
        url.set_path(&format!("{path}/"));
    }
    url.join(path)
        .map_err(|_| SafeError::validation("PROVIDER_URL_INVALID", "Provider 路径无效"))
}

/// DNS 解析并拒绝私有/保留地址。
///
/// 放行条件按 `NetworkPolicy`：
///   - loopback 上的明文 http —— 需要 `allow_local_http`；
///   - 私网地址（含主机名解析到私网）—— 需要 `allow_private`（连接的显式开关）。
pub async fn validate_resolved_url(url: &Url, policy: NetworkPolicy) -> Result<(), SafeError> {
    let host = url
        .host_str()
        .ok_or_else(|| SafeError::validation("PROVIDER_URL_NO_HOST", "地址缺少主机"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| SafeError::validation("PROVIDER_URL_NO_PORT", "地址端口无效"))?;
    let addresses: Vec<IpAddr> = lookup_host((host, port))
        .await
        .map_err(|error| {
            /*
             * ⚠️ 这条消息以前是干巴巴的"地址无法安全解析"，读起来像**被 SSRF 守卫拦了**。
             * 实测踩到：预设里的平台地址写成了 `api.runninghub.cn`——**那个域名根本不存在**
             * （DNS NXDOMAIN），而报错让人以为是安全策略或 Key 有问题，排查方向从一开始就错了。
             * 解析失败时**把主机名和底层原因都说出来**：这一句就是排查的全部线索。
             */
            SafeError::validation(
                "PROVIDER_URL_UNRESOLVABLE",
                format!("域名解析失败：{host}:{port}（{error}）——请确认连接里的平台地址是否正确"),
            )
        })?
        .map(|address| address.ip())
        .collect();
    if addresses.is_empty() {
        return Err(SafeError::validation(
            "PROVIDER_URL_NO_ADDRESS",
            "Provider 地址没有可用解析地址",
        ));
    }
    let is_loopback_host = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    let all_private = addresses.iter().all(|ip| is_private_address(*ip));
    for ip in &addresses {
        let local_http = url.scheme() == "http"
            && policy.allow_local_http
            && is_loopback_host
            && ip.is_loopback();
        let private_ok = policy.allow_private && is_private_address(*ip);
        if is_private_address(*ip) && !local_http && !private_ok {
            return Err(SafeError::validation(
                "PROVIDER_URL_RESTRICTED_NETWORK",
                "Provider 地址解析到受限网络",
            ));
        }
    }
    /*
     * 明文 http 只允许打在"确实在私网里"的地址上（含 loopback）。少了这一条，显式开了
     * allow_private 的连接就能被指向公网的明文地址 —— 那不是这个开关的用途。
     *
     * 错误信息里给出**规则**（远程必须 https），不猜用户想干什么：我们不知道他把服务
     * 放在哪、怎么暴露的，列"你该用隧道/反代"那种方案清单既写不全也会过期。
     */
    if url.scheme() == "http" && policy.allow_private && !is_loopback_host && !all_private {
        return Err(SafeError::validation(
            "PROVIDER_URL_INSECURE",
            "Provider 明文地址解析到了公网：远程地址必须使用 https",
        ));
    }
    Ok(())
}

pub fn is_private_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip == Ipv4Addr::UNSPECIFIED
                || (ip.octets()[0] == 169 && ip.octets()[1] == 254)
                /*
                 * 100.64.0.0/10（CGNAT / 共享地址空间）。
                 *
                 * ⚠️ std 的 `is_private()` **不含**这一段（它只认 10/8、172.16/12、192.168/16），
                 * 而 **Tailscale 恰恰用 100.x 分配地址**。少了这条，"云电脑上跑 ComfyUI、
                 * 用 Tailscale 连过去"的 `http://100.x.y.z:8188` 会被判成**公网明文**而拒掉
                 * （`PROVIDER_URL_INSECURE`：明文地址解析到了公网）—— VPN 里的机器被当成公网。
                 *
                 * 它和 RFC1918 一样在公网不可路由，所以放进"私网"与这个开关的用途一致
                 * （仍然要连接里显式 `allowPrivateNetwork`，不是默认放开）。
                 */
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00 == 0xfc00)
                || (ip.segments()[0] & 0xffc0 == 0xfe80)
        }
    }
}

/// 结果下载地址必须与当前连接同源，且路径匹配允许的路径（用于 ComfyUI /view 等）。
pub fn validate_same_origin(base: &Url, raw: &str, allowed_path: &str) -> Result<Url, SafeError> {
    let url = Url::parse(raw)
        .map_err(|_| SafeError::validation("PROVIDER_RESULT_URL_INVALID", "结果下载地址无效"))?;
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with(allowed_path)
    {
        return Err(SafeError::validation(
            "PROVIDER_RESULT_URL_UNTRUSTED",
            "结果下载地址不是当前连接的允许路径",
        ));
    }
    Ok(url)
}

/// 把 reqwest 的失败翻成**能拿去排查**的错误。
///
/// 为什么要把底层原因与"环境里的出网代理"一起写进去：这三条以前都只说
/// "Provider X 无法连接"——用户看到的是一句没有信息量的话，于是只能猜"是不是 key 坏了"
/// （2026-10-12 实测就是这么问出来的）。而实际原因几乎总在两处：连接根本没建立起来（代理/网络/
/// DNS），或者建起来了但对端没按预期回话。前者必须把代理念出来——reqwest 默认会读
/// `HTTP(S)_PROXY`，代理软件关掉之后它就成了唯一的解释。
pub fn map_network_error(error: &reqwest::Error, operation: &str) -> SafeError {
    let proxy = crate::v2::outbound::configured_proxy();
    let note = crate::v2::outbound::proxy_suffix();
    let mut details = std::collections::BTreeMap::new();
    details.insert("operation".to_string(), serde_json::json!(operation));
    // 原因链（`error.to_string()` 只有最外层那句"error sending request for url …"，
    // 真正有用的"connection refused / dns error / certificate"都在 source 链里）。
    details.insert(
        "cause".to_string(),
        serde_json::json!(cause_chain(error, 300)),
    );
    if let Some(proxy) = proxy {
        details.insert("outboundProxy".to_string(), serde_json::json!(proxy));
    }
    if error.is_timeout() {
        SafeError {
            code: format!("PROVIDER_TIMEOUT_{}", operation.to_uppercase()),
            kind: ErrorKind::Timeout,
            message: format!("{operation} 请求超时{note}"),
            retryable: true,
            details,
        }
    } else if error.is_connect() || error.is_request() {
        SafeError {
            code: format!("PROVIDER_UNREACHABLE_{}", operation.to_uppercase()),
            kind: ErrorKind::Network,
            message: format!("连不上 {operation}{note}"),
            retryable: true,
            details,
        }
    } else {
        SafeError {
            code: format!("PROVIDER_REQUEST_FAILED_{}", operation.to_uppercase()),
            kind: ErrorKind::Network,
            message: format!("{operation} 请求失败{note}"),
            retryable: true,
            details,
        }
    }
}

/// 把错误的原因链拼成一行（`a → b → c`），并按字符数截断——details 是给排查用的，
/// 不该因为一条超长的系统错误把信封撑大。
///
/// ⚠️ 拼之前先过 `domain::redact`：原因链是**唯一**会把完整 URL 带出来的地方，而 URL 里可能有
/// 密钥（rh-proxy 把 key 拼在路径上）。负对照：去掉这一步 ⇒
/// `network_errors_do_not_leak_a_key_embedded_in_the_url` 会红。
fn cause_chain(error: &dyn std::error::Error, limit: usize) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(inner) = source {
        parts.push(inner.to_string());
        source = inner.source();
    }
    let joined = crate::v2::domain::redact::redact_urls(&parts.join(" → "));
    if joined.chars().count() <= limit {
        return joined;
    }
    let mut truncated: String = joined.chars().take(limit).collect();
    truncated.push('…');
    truncated
}

pub fn map_http_error(status: u16, fallback: &str) -> SafeError {
    let retryable = status == 429 || status >= 500;
    let code = match status {
        400 | 401 | 403 | 404 | 405 | 422 => "PROVIDER_REJECTED",
        429 => "PROVIDER_RATE_LIMITED",
        _ => "PROVIDER_UNAVAILABLE",
    };
    SafeError {
        code: code.to_string(),
        kind: ErrorKind::Provider,
        message: format!("Provider 返回 HTTP {status}：{fallback}"),
        retryable,
        details: Default::default(),
    }
}

/// 流式读取响应字节，限制大小。
pub async fn read_limited(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, SafeError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(SafeError::validation(
            "PROVIDER_RESPONSE_TOO_LARGE",
            "Provider 响应超过大小限制",
        ));
    }
    let mut stream = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|_| SafeError::network("PROVIDER_READ_FAILED", "无法读取 Provider 响应"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(SafeError::validation(
                "PROVIDER_RESPONSE_TOO_LARGE",
                "Provider 响应超过大小限制",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// `SafeHttpClient` 的 reqwest 实现：产物/结果 URL 的受限拉取。
pub struct ReqwestSafeHttpClient;

#[async_trait::async_trait]
impl crate::v2::ports::safety::SafeHttpClient for ReqwestSafeHttpClient {
    async fn fetch_limited(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, SafeError> {
        let parsed = validate_base_url(url, NetworkPolicy::STRICT)?;
        validate_resolved_url(&parsed, NetworkPolicy::STRICT).await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let response = client
            .get(parsed.clone())
            .send()
            .await
            .map_err(|error| map_network_error(&error, "artifact_fetch"))?;
        // 关闭重定向后仍被带往别的地址即视为未授权跳转。
        if response.url() != &parsed {
            return Err(SafeError::validation(
                "ARTIFACT_URL_REDIRECTED",
                "产物下载发生未授权重定向",
            ));
        }
        if !response.status().is_success() {
            return Err(map_http_error(response.status().as_u16(), "产物拉取失败"));
        }
        read_limited(response, max_bytes).await
    }

    fn max_result_bytes(&self) -> usize {
        MAX_RESULT_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// 私网判定必须认 VPN 用的 CGNAT 段 —— 否则"云电脑 + Tailscale"的
    /// `http://100.x.y.z:8188` 会被当成公网明文拒掉。
    #[test]
    fn cgnat_counts_as_private_because_tailscale_uses_it() {
        for ip in ["100.64.0.1", "100.101.102.103", "100.127.255.254"] {
            let parsed: Ipv4Addr = ip.parse().unwrap();
            assert!(
                is_private_address(IpAddr::V4(parsed)),
                "{ip} 属于 100.64.0.0/10（CGNAT，Tailscale 用这段），应判为私网"
            );
        }
    }

    /// 负对照：**公网地址绝不能被放进来**。少了这条，上面那个测试即使把
    /// `is_private_address` 改成恒真也照样绿。
    #[test]
    fn public_addresses_stay_public() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255", // CGNAT 段的下边界外
            "100.128.0.1",    // CGNAT 段的上边界外
            "172.32.0.1",     // 172.16/12 之外
            "192.169.0.1",    // 192.168/16 之外
        ] {
            let parsed: Ipv4Addr = ip.parse().unwrap();
            assert!(
                !is_private_address(IpAddr::V4(parsed)),
                "{ip} 是公网地址，不该被判为私网"
            );
        }
        assert!(!is_private_address(IpAddr::V6(
            "2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap()
        )));
    }

    /// 原本就认的几段别被改坏。
    #[test]
    fn classic_private_ranges_still_work() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.50",
            "169.254.1.1",
        ] {
            let parsed: Ipv4Addr = ip.parse().unwrap();
            assert!(is_private_address(IpAddr::V4(parsed)), "{ip} 应判为私网");
        }
    }

    /// 连不上的错误必须**能拿去排查**：说清是哪个操作，并把原因链放进 details。
    ///
    /// 起因是一次真实排查（2026-10-12）：界面上只有一句"连接测试失败"，用户只能猜
    /// "是不是 key 坏了"——而真正的原因是请求走了一个没在跑的本地代理。错误信息里
    /// 没有操作名、没有原因、没有代理，等于把排查成本全推给用户。
    #[tokio::test]
    async fn unreachable_error_names_the_operation_and_carries_the_cause() {
        // 占一个端口再放掉：那个端口上几乎肯定没人听，连接会被立刻拒绝。
        // 比"挑一个常用端口、指望它没被占"确定得多。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("应能占用端口");
        let port = listener.local_addr().expect("应能读出端口").port();
        drop(listener);
        // 显式 no_proxy：这条用例测的是"连不上"本身，不该受运行环境里代理配置的影响。
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("应能建客户端");
        let error = client
            .get(format!("http://127.0.0.1:{port}/system_stats"))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .expect_err("没人听的端口应当连不上");

        let mapped = map_network_error(&error, "probe_status");
        assert_eq!(mapped.code, "PROVIDER_UNREACHABLE_PROBE_STATUS");
        assert_eq!(mapped.kind, ErrorKind::Network);
        assert!(
            mapped.message.starts_with("连不上 probe_status"),
            "错误里要说清是哪个操作：{}",
            mapped.message
        );
        assert!(mapped.retryable, "连不上属于可重试");
        assert_eq!(
            mapped.details.get("operation"),
            Some(&serde_json::json!("probe_status"))
        );
        let cause = mapped
            .details
            .get("cause")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(!cause.is_empty(), "原因链不能是空的（排查全靠它）");
    }

    /// **密钥不许出现在错误详情里**——包括"密钥被拼进 URL"的那种情况。
    ///
    /// rh-proxy 的 ComfyUI 连接把密钥拼在路径上（`{base}/proxy/{key}`），而 reqwest 的错误串
    /// 形如 `error sending request for url (https://host/proxy/<key>)`——不脱敏的话，密钥会进
    /// `details.cause` ⇒ 落进 `v2_runs.error_json` ⇒ 界面 ⇒ **用户贴给别人的任何东西**
    /// （诊断、日志、任务详情）。那正好把"密钥不出凭据库"这条边界从后面捅穿。
    ///
    /// 负对照：把 `cause_chain` 里的 `redact_urls` 去掉 ⇒ 这条会红。
    #[tokio::test]
    async fn network_errors_do_not_leak_a_key_embedded_in_the_url() {
        const KEY: &str = "LEAKPROBE-0123456789abcdef";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("应能占用端口");
        let port = listener.local_addr().expect("应能读出端口").port();
        drop(listener);
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("应能建客户端");
        let error = client
            .get(format!("http://127.0.0.1:{port}/proxy/{KEY}/system_stats"))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .expect_err("没人听的端口应当连不上");

        let mapped = map_network_error(&error, "comfyui_status");
        let rendered = serde_json::to_string(&mapped.details).expect("details 应能序列化");
        assert!(
            !rendered.contains(KEY),
            "密钥被写进了错误详情，它会一路进 DB / 界面 / 用户贴出来的东西：{rendered}"
        );
        // 反向控制：脱敏**不能把整个原因链抹掉**——排查全靠它。
        let cause = mapped
            .details
            .get("cause")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(!cause.is_empty(), "原因链不能是空的（排查全靠它）");
        assert!(
            cause.contains("127.0.0.1") || cause.contains("connection") || cause.contains("拒绝"),
            "主机与底层原因应当留下：{cause}"
        );
    }

    /// 原因链按字符截断（details 是人看的，不该被一条超长的系统错误撑爆）。
    #[test]
    fn cause_chain_is_truncated_by_characters_not_bytes() {
        #[derive(Debug)]
        struct Layer(String);
        impl std::fmt::Display for Layer {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
        impl std::error::Error for Layer {}

        // 中文：按字节截断会切出半个字符（这里每字 3 字节，14 个字 = 42 字节 > 20）
        let deep = Layer("深".repeat(40));
        assert_eq!(cause_chain(&deep, 10), "深深深深深深深深深深…");
        let shallow = Layer("短".to_string());
        assert_eq!(cause_chain(&shallow, 10), "短");
    }
}
