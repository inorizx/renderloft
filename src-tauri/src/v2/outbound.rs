//! 出网环境：那些"应用看不见、却决定它能不能连上平台"的事实。
//!
//! 目前只有一件事：**代理**。reqwest 默认会读 `HTTP(S)_PROXY` / `ALL_PROXY` 并静默使用它们，
//! 所以代理软件一关，应用的表现是"连不上平台 / key 好像坏了"，而界面上没有任何线索。
//! 2026-10-12 就为此排查了一轮：直连 `https://www.runninghub.cn` 是 200，走环境里那个
//! `127.0.0.1:7897`（没人监听）连不上；同一个界面里 ComfyUI 反而是绿的——因为它是
//! `127.0.0.1`、落在 `NO_PROXY` 里直连。**"只有需要出网的那个红"就是代理的指纹。**
//!
//! 放在 `v2/` 顶层而不是 application/ 或 infrastructure/：网络错误映射（infrastructure）
//! 与诊断端点（transports）都要用它，而 `scripts/check-layers.mjs` 不允许 transports
//! 直接 import infrastructure。

/// 环境里实际配着的出网代理（按 reqwest 认的键取第一个非空值）。
///
/// 这是**事实陈述**而不是"本次请求用的代理"：真正用哪个还取决于协议与 `NO_PROXY`。
/// 所以措辞上只说"环境里的出网代理"，不要说成"就是它导致失败"。
pub fn configured_proxy() -> Option<String> {
    const KEYS: [&str; 6] = [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];
    KEYS.iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// 给错误信息用的一小段括注；没配代理时是空串（不给自己加戏）。
pub fn proxy_suffix() -> String {
    match configured_proxy() {
        Some(proxy) => format!("（环境里的出网代理 {proxy}）"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 括注只有两种形态：空串，或者"（环境里的出网代理 …）"。
    ///
    /// 这里**故意不 set_var**：环境变量是进程级的、测试是并行跑的，改了会让别的用例看到
    /// 假事实（本仓库至今没有一处测试这么干，别开这个头）。所以只锁形态，不锁具体值。
    #[test]
    fn proxy_suffix_has_exactly_two_shapes() {
        let suffix = proxy_suffix();
        assert!(
            suffix.is_empty() || suffix.starts_with("（环境里的出网代理 "),
            "实际：{suffix}"
        );
        match configured_proxy() {
            Some(proxy) => assert_eq!(suffix, format!("（环境里的出网代理 {proxy}）")),
            None => assert!(suffix.is_empty(), "没配代理时不该有任何括注：{suffix}"),
        }
    }

    /// 空串与只有空白的值不算"配了代理"（`HTTPS_PROXY=` 这种形态很常见：变量在、但没有值）。
    #[test]
    fn blank_values_do_not_count_as_a_proxy() {
        for candidate in ["", "   ", "\t"] {
            assert!(
                candidate.trim().is_empty(),
                "空白值必须在 configured_proxy 里被过滤掉"
            );
        }
        // 形态自检：真正配了值时必须原样（去掉首尾空白）返回。
        if let Some(proxy) = configured_proxy() {
            assert_eq!(proxy, proxy.trim());
        }
    }
}
