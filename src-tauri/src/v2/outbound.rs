//! 出网环境：那些"应用看不见、却决定它能不能连上平台"的事实。
//!
//! 目前只有一件事：**代理**。来源只有两个，优先级从高到低：
//!
//! 1. **环境变量**（`HTTP(S)_PROXY` / `ALL_PROXY`）——跨工具通用的约定，curl / git / npm 都认；
//! 2. **Windows 的「Internet 选项」系统代理**（注册表 `ProxyEnable` / `ProxyServer` /
//!    `ProxyOverride`）——reqwest **不读**它。于是「Clash 开着系统代理、没设环境变量」这种
//!    极常见的机器上，浏览器打不开的站它能打开、而 RenderLoft 连不上平台，界面却只说
//!    "连接测试失败"。2026-10-26 实测：`ProxyEnable=1 / ProxyServer=127.0.0.1:7897`、
//!    环境变量全空；直连 `google.com` 超时，走代理 302 ⇒ 缺的就是这一条。
//!
//! **只读不写**：这里从不修改注册表，也不替用户决定"该不该用代理"——它只是把系统已经
//! 声明的那条出网路线读出来用。读不到、读不懂（socks / PAC）就当没有，退回直连。
//!
//! 两个来源最后都落成**同一份** `ExplicitProxy`，由同一套判据决定每条请求走不走：
//! 回环永远直连、`NO_PROXY` / `ProxyOverride` 一律生效。**不把环境变量那一半留给 reqwest**：
//! 装了自定义代理之后它不再自己读环境变量（`auto_sys_proxy` 被关掉），这样"这次用的到底
//! 是哪个代理"只有一份答案，回环豁免也不会看来源下菜。
//!
//! 放在 `v2/` 顶层而不是 application/ 或 infrastructure/：网络错误映射（infrastructure）
//! 与诊断端点（transports）都要用它，而 `scripts/check-layers.mjs` 不允许 transports
//! 直接 import infrastructure。

/// 代理从哪来。**只用于说人话**（日志与错误括注），不做任何决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    Environment,
    /// Windows 系统代理（注册表）
    System,
}

impl ProxySource {
    /// 给中文错误信息/日志用的说法。
    fn label(self) -> &'static str {
        match self {
            ProxySource::Environment => "环境变量",
            ProxySource::System => "系统代理",
        }
    }
}

/// 一份显式代理配置（目前只有系统代理这一种来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitProxy {
    pub source: ProxySource,
    /// 所有协议共用一个代理（`ProxyServer=127.0.0.1:7897` 这种写法）
    pub all: Option<String>,
    pub http: Option<String>,
    pub https: Option<String>,
    /// 直连模式（原样保存，匹配时再解释）
    pub bypass: Vec<String>,
    /// Windows 的 `<local>`：**没有点的主机名**直连（内网短名）
    pub bypass_local: bool,
    /// 认得出、但这一版用不了的条目（socks / PAC）。
    ///
    /// 只在"没有可用地址"时进错误括注：让用户知道**我们看见了他的配置**，只是不支持，
    /// 而不是静默直连、看起来像配置没生效。
    pub unsupported: Vec<String>,
}

impl ExplicitProxy {
    /// 空配置。`source` 必须显式给：一个"不知道谁给的"代理没法在错误信息里说清楚。
    pub fn new(source: ProxySource) -> Self {
        Self {
            source,
            all: None,
            http: None,
            https: None,
            bypass: Vec::new(),
            bypass_local: false,
            unsupported: Vec::new(),
        }
    }

    /// 诊断/日志里显示哪个地址（有一个就够）。
    pub fn display_url(&self) -> Option<&str> {
        self.all
            .as_deref()
            .or(self.https.as_deref())
            .or(self.http.as_deref())
    }

    /// **这条请求要不要走代理、走哪个**（`None` = 直连）。纯判据，可单测。
    pub fn proxy_for(&self, scheme: &str, host: &str) -> Option<&str> {
        if bypasses(host, &self.bypass, self.bypass_local) {
            return None;
        }
        match scheme.to_ascii_lowercase().as_str() {
            "https" => self.https.as_deref().or(self.all.as_deref()),
            "http" => self.http.as_deref().or(self.all.as_deref()),
            _ => self.all.as_deref(),
        }
    }
}

/// 这次进程该按哪种方式出网。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyPlan {
    /// 有代理：由**我们自己的判据**决定每条请求走不走、走哪个
    /// （回环永远直连、`NO_PROXY` / `ProxyOverride` 一律生效）。
    Explicit(ExplicitProxy),
    /// 不用代理
    Off,
}

/// 环境变量的一次快照（抽出来是为了让决定逻辑可测：测试里不能改进程环境）。
#[derive(Debug, Clone, Default)]
pub struct EnvironmentView {
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub all_proxy: Option<String>,
    pub no_proxy: Option<String>,
}

impl EnvironmentView {
    pub fn current() -> Self {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            https_proxy: read("HTTPS_PROXY").or_else(|| read("https_proxy")),
            http_proxy: read("HTTP_PROXY").or_else(|| read("http_proxy")),
            all_proxy: read("ALL_PROXY").or_else(|| read("all_proxy")),
            no_proxy: read("NO_PROXY").or_else(|| read("no_proxy")),
        }
    }

    /// 环境变量的代理也落成同一份 `ExplicitProxy`。
    ///
    /// 为什么不直接"交给 reqwest 读"（这里踩过）：那样回环豁免就只对系统代理生效——
    /// 设着 `HTTPS_PROXY` 的机器上，本机 ComfyUI 的请求照样会被交给代理（2026-10-26 实测
    /// 两条测试因此变红）。**一份判据管所有来源**，回环与 `NO_PROXY` 才不会看来源下菜。
    ///
    /// 只给了 `socks://` 时同样返回一个"显式但没有可用地址"的方案：让错误括注能说出
    /// "环境变量 只给了 socks=…，用不了"，而不是静默直连。
    pub fn as_proxy(&self) -> Option<ExplicitProxy> {
        let mut proxy = ExplicitProxy::new(ProxySource::Environment);
        let mut unsupported = Vec::new();
        for (label, value, slot) in [
            ("http", &self.http_proxy, &mut proxy.http),
            ("https", &self.https_proxy, &mut proxy.https),
            ("all", &self.all_proxy, &mut proxy.all),
        ] {
            let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
                continue;
            };
            match normalize_proxy_url(value) {
                Some(url) => *slot = Some(url),
                None => unsupported.push(format!("{label}={value}")),
            }
        }
        if proxy.display_url().is_none() && unsupported.is_empty() {
            return None;
        }
        proxy.unsupported = unsupported;
        Some(with_no_proxy(proxy, self.no_proxy.as_deref()))
    }
}

/// 决定"这次按哪种方式出网"。**纯函数**：环境与系统代理都从参数进来。
///
/// 优先级：环境变量 > 系统代理 > 直连。环境变量排在系统代理前面是**不改变既有行为**：
/// 今天吃着环境变量的人，升级后走的还是同一条路（只是回环与 `NO_PROXY` 现在真的生效）。
pub fn plan_from(env: &EnvironmentView, system: Option<ExplicitProxy>) -> ProxyPlan {
    if let Some(proxy) = env.as_proxy() {
        return ProxyPlan::Explicit(proxy);
    }
    match system {
        // 注意：这里**不看有没有地址**。只有 socks / PAC 的系统代理同样会走到这，
        // 它的价值是"错误信息里能说出为什么没生效"，而不是"没有代理"。
        Some(system) => ProxyPlan::Explicit(with_no_proxy(system, env.no_proxy.as_deref())),
        None => ProxyPlan::Off,
    }
}

/// 把环境里的 `NO_PROXY` 并进系统代理的绕过表。
///
/// 为什么：`NO_PROXY` 是**跨工具通用的约定**（curl / git / npm 都认），而 Windows 的
/// `ProxyOverride` 只管得到用它自己那份配置的应用。用户既然写了一份"这些别走代理"，
/// 就不该因为"我们这次读的是注册表"而失效。
fn with_no_proxy(mut proxy: ExplicitProxy, raw: Option<&str>) -> ExplicitProxy {
    let Some(raw) = raw else {
        return proxy;
    };
    let (mut patterns, local) = split_bypass(raw);
    proxy.bypass.append(&mut patterns);
    proxy.bypass_local |= local;
    proxy
}

/// 当前该用哪种方式出网。**每次都重新算**，不缓存。
///
/// 为什么不缓存（这里踩过）：系统代理是**活的**——用户完全可能先开应用、再开代理软件
/// （或反过来关掉）。缓存一次的话，请求还按旧代理走，用户只能重启。浏览器不会这样：
/// 系统设置一变，下一次请求就跟上了。一次解析 = 两三次注册表读取（几十微秒），
/// 对比一次网络请求可以忽略。
pub fn resolve() -> ProxyPlan {
    plan_from(&EnvironmentView::current(), system_proxy())
}

/// Windows 系统代理；其它平台与"没开"都是 `None`。
#[cfg(windows)]
pub fn system_proxy() -> Option<ExplicitProxy> {
    let enabled = registry::read_dword("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        // PAC（自动配置脚本）不会去执行它——认得出就记一笔，别假装没有。
        if registry::read_string("AutoConfigURL").is_some() {
            let mut proxy = ExplicitProxy::new(ProxySource::System);
            proxy.unsupported.push("PAC".to_string());
            return Some(proxy);
        }
        return None;
    }
    let server = registry::read_string("ProxyServer")?;
    let override_list = registry::read_string("ProxyOverride").unwrap_or_default();
    let parsed = parse_registry_proxy(&server, &override_list);
    if parsed.display_url().is_none() && parsed.unsupported.is_empty() {
        return None;
    }
    Some(parsed)
}

#[cfg(not(windows))]
pub fn system_proxy() -> Option<ExplicitProxy> {
    // Linux / macOS 只吃环境变量（与 reqwest 的默认行为一致）。
    None
}

/// Windows 注册表读取：`HKCU\…\Internet Settings` 下的三格。
///
/// 为什么不引一个注册表 crate：这里只要读两个字符串 + 一个 DWORD，而 `windows-sys`
/// 已经在依赖里（托盘/单实例那几处 FFI 用的就是它）。**只读 HKCU**、不加新依赖。
#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    const INTERNET_SETTINGS: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 读一个 `REG_SZ`（空串与不存在都返回 `None`：对代理来说它们是同一件事）。
    pub fn read_string(name: &str) -> Option<String> {
        let subkey = wide(INTERNET_SETTINGS);
        let value = wide(name);
        // 第一次问长度（`pvdata` 传空就是"只问要多大"），第二次才真的读。
        let mut size: u32 = 0;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS || size < 2 {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
        let mut size = (buffer.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..length]);
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    /// 读一个 `REG_DWORD`（`ProxyEnable` 就是它）。
    pub fn read_dword(name: &str) -> Option<u32> {
        let subkey = wide(INTERNET_SETTINGS);
        let value = wide(name);
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(data)
    }
}

/// 解析注册表里的 `ProxyServer` + `ProxyOverride`（**纯函数**，可在任何平台单测）。
///
/// `ProxyServer` 两种写法都真实存在：
/// * `127.0.0.1:7897` —— 所有协议一个口；
/// * `http=127.0.0.1:7897;https=127.0.0.1:7897` —— 分协议（`socks=` 这一版用不了）。
pub fn parse_registry_proxy(server: &str, override_list: &str) -> ExplicitProxy {
    let (bypass, bypass_local) = split_bypass(override_list);
    let mut proxy = ExplicitProxy::new(ProxySource::System);
    proxy.bypass = bypass;
    proxy.bypass_local = bypass_local;
    let server = server.trim();
    if server.contains('=') {
        for entry in server.split(';') {
            let Some((scheme, value)) = entry.split_once('=') else {
                continue;
            };
            let scheme = scheme.trim().to_ascii_lowercase();
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match scheme.as_str() {
                "http" => proxy.http = normalize_proxy_url(value),
                "https" => proxy.https = normalize_proxy_url(value),
                // reqwest 没有启用 socks 特性；认出来是为了**说出来**，而不是静默直连。
                "socks" | "socks4" | "socks4a" | "socks5" | "socks5h" | "ftp" => {
                    proxy.unsupported.push(format!("{scheme}={value}"))
                }
                _ => proxy.unsupported.push(format!("{scheme}={value}")),
            }
        }
    } else {
        proxy.all = normalize_proxy_url(server);
    }
    proxy
}

/// 补上协议、去掉尾部斜杠；`socks*` 这种用不了，返回 `None` 并交给调用方记账。
fn normalize_proxy_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let lowered = value.to_ascii_lowercase();
    if lowered.starts_with("socks") {
        return None;
    }
    let with_scheme = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    Some(with_scheme.trim_end_matches('/').to_string())
}

/// 把 `ProxyOverride` / `NO_PROXY` 那种分号/逗号列表拆成模式；`<local>` 单独成一个开关。
fn split_bypass(list: &str) -> (Vec<String>, bool) {
    let mut patterns = Vec::new();
    let mut local = false;
    for raw in list.split([';', ',']) {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        if item.eq_ignore_ascii_case("<local>") {
            local = true;
            continue;
        }
        patterns.push(item.to_ascii_lowercase());
    }
    (patterns, local)
}

/// 这个主机要不要**绕过**代理直连。纯判据，两个层次：
///
/// 1. **回环地址永远直连**（`localhost` / `127.0.0.0/8` / `::1`）—— 与 Chromium 一致：
///    它默认就是 `<-loopback>` 未开的状态。本机 ComfyUI 不能因为机器上开着代理就变红；
/// 2. 其余一律**听系统的绕过列表**（`ProxyOverride` / `NO_PROXY`），`<local>` 按 Windows 的
///    原义展开成"没有点的主机名"。
///
/// ⚠️ **不替用户发明规则**：这里曾经硬编码"私有网段（`10.*` / `192.168.*`）与 `.local`
/// 一律直连"，理由是"公网代理到不了它们、别把内网 ComfyUI 弄挂"。但那是错的取舍——
/// 企业内网走代理（PAC / 内网代理）是真实用法，我们一绕过就等于替用户否掉了它。
/// 主流软件（Chromium / Windows 自身）只在**回环**上做隐式例外，其它交给配置。
/// 内网 ComfyUI 的绕过由代理软件自己写进 `ProxyOverride`（Clash 就写了 20 条）。
pub fn bypasses(host: &str, patterns: &[String], bypass_local: bool) -> bool {
    let host = normalize_host(host);
    if host.is_empty() {
        return false;
    }
    if is_loopback(&host) {
        return true;
    }
    if bypass_local && !host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        return true;
    }
    patterns.iter().any(|pattern| pattern_matches(&host, pattern))
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn is_loopback(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// 通配匹配（只认 `*`）。`*.example.com` 也匹配裸域 `example.com`——**人的直觉优先于
/// WinINET 的字面语义**，而且"我明明写了绕过它却还是走了代理"比"多绕过了一台"更糟。
fn pattern_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.');
    if pattern.is_empty() {
        return false;
    }
    if let Some(rest) = pattern.strip_prefix("*.") {
        if host == rest {
            return true;
        }
    }
    if !pattern.contains('*') {
        return host == pattern;
    }
    glob_match(host, pattern)
}

fn glob_match(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < text.len() {
        if pi < pattern.len() && pattern[pi] == text[ti] {
            ti += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(star_at) = star {
            pi = star_at + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

// ---------- 给诊断 / 错误信息用的两小块 ----------

/// 诊断里"出网代理"那一格：**实际会用的那个**（有就报，没有就是直连）。
pub fn configured_proxy() -> Option<String> {
    match resolve() {
        ProxyPlan::Explicit(proxy) => proxy.display_url().map(str::to_string),
        ProxyPlan::Off => None,
    }
}

/// 给日志用的一句话：谁给的代理、地址、绕开了几条。
///
/// 与 `proxy_suffix()` 分开：括注要短（它跟在错误信息后面），日志要全（出问题第一眼看它）。
pub fn describe() -> String {
    match resolve() {
        ProxyPlan::Explicit(proxy) => {
            let mut text = match proxy.display_url() {
                Some(url) => format!("{} {url}", proxy.source.label()),
                None => format!("{}（没有可用的地址）", proxy.source.label()),
            };
            if !proxy.bypass.is_empty() {
                text.push_str(&format!("，绕过 {} 条", proxy.bypass.len()));
            }
            if proxy.bypass_local {
                text.push_str("，含 <local>");
            }
            if !proxy.unsupported.is_empty() {
                text.push_str(&format!("；用不了：{}", proxy.unsupported.join("；")));
            }
            text
        }
        ProxyPlan::Off => "无（直连）".to_string(),
    }
}

/// 给错误信息用的一小段括注；没有代理时是空串（不给自己加戏）。
///
/// 带上来源是刻意的：用户看到"系统代理"会去关代理软件，看到"环境变量"会去查 shell。
/// 系统代理只给了 socks / PAC 时也说话——否则就是"配置明明在，却静默失效"。
pub fn proxy_suffix() -> String {
    match resolve() {
        ProxyPlan::Off => String::new(),
        ProxyPlan::Explicit(proxy) => match proxy.display_url() {
            Some(url) => format!("（出网代理 {url} · {}）", proxy.source.label()),
            None if !proxy.unsupported.is_empty() => format!(
                "（{} 只给了 {}，用不了）",
                proxy.source.label(),
                proxy.unsupported.join("、")
            ),
            None => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这台机器上的真实值（2026-10-26 实测，Clash 写进去的）：一个口 + 一长串绕过。
    const REAL_SERVER: &str = "127.0.0.1:7897";
    const REAL_OVERRIDE: &str = "localhost;127.*;192.168.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;<local>";

    #[test]
    fn a_single_registry_server_covers_every_scheme() {
        let proxy = parse_registry_proxy(REAL_SERVER, REAL_OVERRIDE);
        assert_eq!(proxy.source, ProxySource::System);
        assert_eq!(proxy.display_url(), Some("http://127.0.0.1:7897"));
        assert!(proxy.bypass_local, "`<local>` 要变成开关，不是一条模式");
        assert!(
            !proxy.bypass.iter().any(|item| item == "<local>"),
            "`<local>` 不能留在模式表里（它匹配不到任何主机名）"
        );
        assert_eq!(
            proxy.proxy_for("https", "www.runninghub.cn"),
            Some("http://127.0.0.1:7897")
        );
        assert_eq!(
            proxy.proxy_for("http", "www.runninghub.cn"),
            Some("http://127.0.0.1:7897")
        );
    }

    #[test]
    fn a_keyed_registry_server_splits_by_scheme() {
        let proxy = parse_registry_proxy("http=127.0.0.1:7890;https=127.0.0.1:7891", "localhost");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(proxy.https.as_deref(), Some("http://127.0.0.1:7891"));
        assert_eq!(proxy.all, None, "分协议写法不该再有一个 all");
        assert_eq!(proxy.proxy_for("https", "a.com"), Some("http://127.0.0.1:7891"));
        assert_eq!(proxy.proxy_for("http", "a.com"), Some("http://127.0.0.1:7890"));
        // 没列出来的协议 → 直连（Windows 的语义）
        assert_eq!(proxy.proxy_for("ftp", "a.com"), None);
    }

    /// socks 用不了就得**说出来**：静默直连会让用户以为代理生效了，然后看着它连不上。
    #[test]
    fn socks_entries_are_reported_as_unsupported_instead_of_silently_direct() {
        let proxy = parse_registry_proxy("socks=127.0.0.1:1080", "");
        assert_eq!(proxy.display_url(), None);
        assert!(
            proxy
                .unsupported
                .iter()
                .any(|item| item.contains("socks=127.0.0.1:1080")),
            "socks 必须进 unsupported：{:?}",
            proxy.unsupported
        );
        assert_eq!(proxy.proxy_for("https", "a.com"), None);
    }

    /// 只有 socks 的系统代理：方案是"显式但没地址"，错误括注要说清是它用不了。
    ///
    /// 这条只走纯函数，不碰本机注册表——判据是括注的**形状**，不是这台机器开没开代理。
    #[test]
    fn a_socks_only_system_proxy_explains_itself_in_the_error_suffix() {
        let system = parse_registry_proxy("socks=127.0.0.1:1080", "");
        let plan = plan_from(&EnvironmentView::default(), Some(system));
        match &plan {
            ProxyPlan::Explicit(proxy) => assert!(proxy.display_url().is_none()),
            other => panic!("应当是显式但无地址，实际 {other:?}"),
        }
        let text = match plan {
            ProxyPlan::Explicit(proxy) => format!(
                "（{} 只给了 {}，用不了）",
                proxy.source.label(),
                proxy.unsupported.join("、")
            ),
            other => panic!("应当是显式方案，实际 {other:?}"),
        };
        assert_eq!(text, "（系统代理 只给了 socks=127.0.0.1:1080，用不了）");
    }

    /// 环境变量只给了 `socks://`：同样是"显式但没有可用地址"，错误括注要说明。
    #[test]
    fn a_socks_only_environment_proxy_explains_itself_too() {
        let env = EnvironmentView {
            all_proxy: Some("socks5://127.0.0.1:1080".to_string()),
            ..Default::default()
        };
        match plan_from(&env, None) {
            ProxyPlan::Explicit(proxy) => {
                assert_eq!(proxy.source, ProxySource::Environment);
                assert!(proxy.display_url().is_none());
                assert_eq!(
                    proxy.unsupported,
                    vec!["all=socks5://127.0.0.1:1080".to_string()]
                );
            }
            other => panic!("应当报出来而不是静默直连，实际 {other:?}"),
        }
    }

    #[test]
    fn a_scheme_less_value_gets_http_and_a_trailing_slash_is_dropped() {
        let proxy = parse_registry_proxy("127.0.0.1:7897/", "");
        assert_eq!(proxy.display_url(), Some("http://127.0.0.1:7897"));
        let proxy = parse_registry_proxy("http://127.0.0.1:7897", "");
        assert_eq!(proxy.display_url(), Some("http://127.0.0.1:7897"));
    }

    /// **只有回环是隐式直连**，其它一律听系统的绕过列表——与 Chromium / Windows 同一条规矩。
    ///
    /// 这条判据的正反两面都要钉住：回环（本机 ComfyUI 就住那儿）必须永远直连；
    /// **私有网段不能**被我们擅自绕过——企业内网走代理是真实用法，绕过等于替用户否决它。
    /// 负对照：把 `is_loopback` 换成"回环 + 私有网段"，下面第二条断言就红。
    #[test]
    fn only_loopback_bypasses_without_a_pattern() {
        for host in ["127.0.0.1", "127.1.2.3", "localhost", "LOCALHOST", "::1", "[::1]"] {
            assert!(bypasses(host, &[], false), "回环必须直连：{host}");
        }
        for host in [
            "www.runninghub.cn",
            "8.8.8.8",
            // 私有网段：**没有**写在绕过列表里就该走代理（主流语义）
            "10.0.0.8",
            "192.168.1.20",
            "172.16.5.5",
            "169.254.1.1",
            "gpu-box.local",
            "172.32.0.1",
        ] {
            assert!(!bypasses(host, &[], false), "不该被隐式绕过：{host}");
        }
    }

    /// 但"写在列表里"的私有网段当然要直连——这正是 Clash 往注册表里写那 20 条的意思。
    #[test]
    fn a_private_range_bypasses_when_the_system_says_so() {
        let (patterns, local) = split_bypass(REAL_OVERRIDE);
        assert!(bypasses("192.168.1.20", &patterns, local));
        assert!(bypasses("10.1.2.3", &patterns, local));
        assert!(bypasses("172.20.0.9", &patterns, local));
        assert!(
            !bypasses("www.runninghub.cn", &patterns, local),
            "公网地址不在列表里，必须走代理"
        );
    }

    /// 环境里的 `NO_PROXY` 要并进系统代理的绕过表。
    ///
    /// 为什么：`NO_PROXY` 是**跨工具通用**的约定（curl / git / npm 都认），而 Windows 的
    /// `ProxyOverride` 只管得到用它自己那份配置的应用。用户写了一份"这些别走代理"，
    /// 不该因为"我们这次读的是注册表"而失效。
    #[test]
    fn no_proxy_is_merged_into_the_system_bypass_list() {
        let system = parse_registry_proxy(REAL_SERVER, "localhost");
        let merged = with_no_proxy(system, Some("*.internal, 10.*;<local>"));
        assert!(bypasses("api.internal", &merged.bypass, merged.bypass_local));
        assert!(bypasses("10.9.9.9", &merged.bypass, merged.bypass_local));
        assert!(merged.bypass_local, "`<local>` 也要并进来");
        assert!(
            bypasses("localhost", &merged.bypass, merged.bypass_local),
            "系统自己那几条不能被顶掉"
        );

        // 决定逻辑真的会调用它（不是只有 helper 对）。
        let env = EnvironmentView {
            no_proxy: Some("*.internal".to_string()),
            ..Default::default()
        };
        match plan_from(&env, Some(parse_registry_proxy(REAL_SERVER, ""))) {
            ProxyPlan::Explicit(proxy) => assert!(
                proxy.proxy_for("https", "api.internal").is_none(),
                "环境里的 NO_PROXY 必须在系统代理模式下也生效"
            ),
            other => panic!("应当是系统代理，实际 {other:?}"),
        }
    }

    #[test]
    fn bypass_patterns_follow_wildcards_and_bare_domains() {
        let patterns = vec![
            "*.example.com".to_string(),
            "exact.test".to_string(),
            "172.16.*".to_string(),
        ];
        assert!(bypasses("api.example.com", &patterns, false));
        assert!(bypasses("example.com", &patterns, false), "`*.x` 也该匹配裸域");
        assert!(bypasses("exact.test", &patterns, false));
        assert!(bypasses("172.16.9.9", &patterns, false));
        assert!(!bypasses("notexample.com", &patterns, false));
        assert!(
            !bypasses("exact.test.evil.com", &patterns, false),
            "精确名不该变成后缀匹配"
        );
    }

    #[test]
    fn local_short_names_bypass_only_with_local_set() {
        assert!(bypasses("gpu-box", &[], true));
        assert!(!bypasses("gpu-box", &[], false));
        assert!(!bypasses("gpu-box.home.arpa", &[], true), "带点的不是内网短名");
    }

    /// 环境变量优先于系统代理，**并且回环豁免对它同样生效**。
    ///
    /// 后半句是 2026-10-26 那次修复的核心：以前环境变量这一半交给 reqwest 自己读，
    /// 于是"设着 HTTPS_PROXY 的机器上，本机 ComfyUI 也被交给代理"。现在两个来源落成
    /// 同一份判据，回环不看来源下菜。
    #[test]
    fn environment_proxy_wins_and_still_bypasses_loopback() {
        let env = EnvironmentView {
            https_proxy: Some("http://env-proxy:8080".to_string()),
            no_proxy: Some("*.internal".to_string()),
            ..Default::default()
        };
        let system = Some(parse_registry_proxy(REAL_SERVER, REAL_OVERRIDE));
        match plan_from(&env, system) {
            ProxyPlan::Explicit(proxy) => {
                assert_eq!(proxy.source, ProxySource::Environment);
                assert_eq!(proxy.proxy_for("https", "www.runninghub.cn"), Some("http://env-proxy:8080"));
                assert_eq!(
                    proxy.proxy_for("https", "127.0.0.1"),
                    None,
                    "环境变量代理也不能把回环带走——本机 ComfyUI 就在那儿"
                );
                assert_eq!(
                    proxy.proxy_for("https", "api.internal"),
                    None,
                    "环境变量里的 NO_PROXY 必须生效"
                );
            }
            other => panic!("应当是环境变量代理，实际 {other:?}"),
        }
    }

    /// 只给了 `ALL_PROXY` 时所有协议都走它（curl 的语义）。
    #[test]
    fn all_proxy_covers_every_scheme() {        let env = EnvironmentView {
            all_proxy: Some("127.0.0.1:7897".to_string()),
            ..Default::default()
        };
        match plan_from(&env, None) {
            ProxyPlan::Explicit(proxy) => {
                assert_eq!(proxy.proxy_for("http", "a.com"), Some("http://127.0.0.1:7897"));
                assert_eq!(proxy.proxy_for("https", "a.com"), Some("http://127.0.0.1:7897"));
            }
            other => panic!("应当是环境变量代理，实际 {other:?}"),
        }
    }

    #[test]
    fn without_any_proxy_the_plan_is_direct() {
        assert_eq!(plan_from(&EnvironmentView::default(), None), ProxyPlan::Off);
    }

    #[test]
    fn the_system_proxy_is_used_when_the_environment_is_empty() {
        let system = Some(parse_registry_proxy(REAL_SERVER, REAL_OVERRIDE));
        match plan_from(&EnvironmentView::default(), system) {
            ProxyPlan::Explicit(proxy) => {
                assert_eq!(proxy.source, ProxySource::System);
                assert_eq!(proxy.display_url(), Some("http://127.0.0.1:7897"));
            }
            other => panic!("系统代理该生效，实际 {other:?}"),
        }
    }

    /// 括注只有三种形态：空串、"（出网代理 … · 来源）"、"（系统代理 只给了 …，用不了）"。
    ///
    /// 这里**故意不 set_var**：环境变量是进程级的、测试是并行跑的，改了会让别的用例看到
    /// 假事实（本仓库至今没有一处测试这么干，别开这个头）。所以只锁形态，不锁具体值。
    #[test]
    fn proxy_suffix_names_the_source_or_says_nothing() {
        let suffix = proxy_suffix();
        assert!(
            suffix.is_empty()
                || (suffix.starts_with("（出网代理 ") && suffix.ends_with('）'))
                || (suffix.starts_with("（系统代理 只给了 ") && suffix.ends_with("，用不了）")),
            "实际：{suffix}"
        );
        match configured_proxy() {
            Some(proxy) => assert!(suffix.contains(&proxy), "括注里必须有那个地址：{suffix}"),
            None => assert!(
                suffix.is_empty() || suffix.starts_with("（系统代理 只给了 "),
                "没有地址时不该报地址：{suffix}"
            ),
        }
    }

    /// 空白的代理值不算"配了代理"（`HTTPS_PROXY=` 这种形态很常见：变量在、但没有值）。
    #[test]
    fn blank_values_do_not_count_as_a_proxy() {
        assert_eq!(
            normalize_proxy_url("   "),
            None,
            "空白值必须在归一化时被丢掉"
        );
        // `EnvironmentView::current` 会过滤空白，但决定逻辑自己也该扛住——
        // 否则一个 `HTTPS_PROXY=` 会把系统代理顶掉，用户看到的又是"连不上"。
        let env = EnvironmentView {
            https_proxy: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(plan_from(&env, None), ProxyPlan::Off);
        let system = Some(parse_registry_proxy(REAL_SERVER, REAL_OVERRIDE));
        assert!(
            matches!(plan_from(&env, system), ProxyPlan::Explicit(_)),
            "空白的环境变量不该挡住系统代理"
        );
    }

    /// Windows 注册表读取：**只验不炸**（值取决于这台机器，不能拿它当判据）。
    #[cfg(windows)]
    #[test]
    fn reading_the_system_proxy_does_not_panic() {
        let _ = system_proxy();
    }
}
