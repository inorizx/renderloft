//! 往外写的文字里，**URL 只留 scheme + host**。
//!
//! 为什么需要它：有的驱动把密钥拼在 URL 上（ComfyUI 的 rh-proxy 模式就是
//! `{base}/proxy/{key}`），而 reqwest 的错误串形如
//! `error sending request for url (https://host/proxy/<key>)`。这些文字会走到三个出口：
//!
//! 1. **错误详情**（`SafeError.details.cause`）⇒ 落进 `v2_runs.error_json` ⇒ 界面；
//! 2. **日志行**（`v2::logging`）⇒ 用户把日志目录交给别人（或他的 agent）去发掘；
//! 3. 用户自己复制粘贴的任何东西（诊断、issue、聊天记录）。
//!
//! 任一处不脱敏，密钥就从后面把"**密钥不出凭据库**"这条边界捅穿（2026-10-13 实测漏过一次）。
//!
//! 砍路径**不砍信息**：是"哪一次调用"由调用方自己的字段说清了（`details.operation`、
//! 日志的 scope），主机与底层错误才是要看的东西。没有路径的 URL（如 `HTTP(S)_PROXY` 那种）
//! 原样保留——它本来就没有秘密，而且是"连不上"的关键线索。
//!
//! 放 domain 而不是某个具体层：错误详情在 infrastructure、日志在 `v2::logging`，
//! 两边**必须**用同一条规则（同一规则留两处，迟早漂）。

/// 把文本里出现的所有 `http(s)://host/...` 砍成 `http(s)://host/…`。
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = match (rest.find("http://"), rest.find("https://")) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(start) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        // URL 的终点。两条判据合起来才既**安全**（不把路径留在外面）又**不吞字**：
        //  * 空白与错误串里的收尾符号（reqwest 就是 `url (...)` 这个形态）；
        //  * **任何非 ASCII 字符**——URL 是 ASCII（中文只会以 `%E4%B8%AD` 的形态出现），
        //    所以中文标点/汉字一定是"URL 已经结束"。
        //    ⚠️ 少了这一条会**悄悄吃掉后面的正文**（我第一版就是这样：`https://h2/p2，后面还有话`
        //    整段被当成一个 URL，只剩 `https://h2/…`）——日志里丢字比不脱敏更难查。
        let end = tail
            .find(|c: char| {
                c.is_whitespace()
                    || !c.is_ascii()
                    || matches!(c, ')' | '"' | '\'' | ',' | ']' | '}')
            })
            .unwrap_or(tail.len());
        out.push_str(&redact_one(&tail[..end]));
        rest = &tail[end..];
    }
}

fn redact_one(url: &str) -> String {
    let Some((scheme, after_scheme)) = url.split_once("://") else {
        return url.to_string();
    };
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    if host.len() == after_scheme.len() {
        // 没有路径（`http://127.0.0.1:7897`）：原样，它本来就是有用的线索。
        url.to_string()
    } else {
        format!("{scheme}://{host}/…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_host_and_drops_the_path() {
        assert_eq!(
            redact_urls(
                "error sending request for url (https://www.runninghub.cn/proxy/SECRET-KEY)"
            ),
            "error sending request for url (https://www.runninghub.cn/…)"
        );
    }

    /// 没有路径的 URL 原样保留：代理地址是"连不上"的关键线索，砍了等于把排查线索砍掉。
    #[test]
    fn keeps_urls_without_a_path() {
        assert_eq!(
            redact_urls("连不上 comfyui_status（环境里的出网代理 http://127.0.0.1:7897）"),
            "连不上 comfyui_status（环境里的出网代理 http://127.0.0.1:7897）"
        );
    }

    /// 一条文本里有多个 URL、且 URL 后面直接跟中文的情况。
    #[test]
    fn handles_multiple_urls_and_trailing_text() {
        assert_eq!(
            redact_urls("a http://h1/p1/k1 → b https://h2/p2，后面还有话"),
            "a http://h1/… → b https://h2/…，后面还有话"
        );
    }

    /// 没有 URL 的文本原样返回（别把正常句子改坏）。
    #[test]
    fn leaves_plain_text_alone() {
        let plain = "远端任务在对账期限内未进入终态";
        assert_eq!(redact_urls(plain), plain);
    }
}
