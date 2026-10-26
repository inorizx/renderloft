//! 把 `v2::outbound` 的决定装到 reqwest 上。
//!
//! 这一层只有两件事：
//!
//! 1. **每一处出网客户端都要从这里过一遍**，否则"代理生效了"就成了只装一半——
//!    最坏的情况是提交走了代理、查询没走，用户看到"任务提交成功，然后永远查不到状态"。
//! 2. **每一次请求都重新问一次**（`Proxy::custom` 的闭包），不是建客户端时问一次。
//!    系统代理是活的：用户很可能先开应用再开代理软件。浏览器改完设置下一次请求就跟上，
//!    我们也一样——否则界面写着新代理、请求还按旧的走，用户只能重启。
//!
//! 明确**不**走代理的一处：daemon 自己的本地客户端（`clients/client.rs`，回环地址）。

use crate::v2::outbound::ProxyPlan;
use std::sync::OnceLock;

/// 建一个**认代理**的客户端 builder。
pub fn configure(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    log_once();
    apply_with(builder, &crate::v2::outbound::resolve(), crate::v2::outbound::resolve)
}

/// 装上某个方案。`resolver` 让判据能单测：真实路径传 `outbound::resolve`，
/// 测试传一个"随时会变"的闭包。
pub fn apply(builder: reqwest::ClientBuilder, plan: &ProxyPlan) -> reqwest::ClientBuilder {
    apply_with(builder, plan, crate::v2::outbound::resolve)
}

pub fn apply_with<F>(
    builder: reqwest::ClientBuilder,
    plan: &ProxyPlan,
    resolver: F,
) -> reqwest::ClientBuilder
where
    F: Fn() -> ProxyPlan + Send + Sync + 'static,
{
    match plan {
        // 交给 reqwest：环境变量代理（连同 `NO_PROXY`）由它自己处理。进程环境不会中途变，
        // 所以这里没有"动态"的必要；插一脚反而会丢掉它认识的写法。
        ProxyPlan::Environment => builder,
        // `no_proxy()` 会**连环境变量里的代理一起压掉**——这是 `RENDERLOFT_PROXY=off`
        // 能救回"环境里有个死代理"那种现场的机制，必须是个静态决定。
        ProxyPlan::Off { disabled: true } => builder.no_proxy(),
        // 其余（系统代理 / 手工指定 / 暂时没有代理）都装同一个动态闭包：
        // 没有代理时它返回 `None`，而**装了自定义代理之后 reqwest 就不再读环境变量**
        // （`client.rs` 里 `auto_sys_proxy` 被关掉），所以"返回 None"是**真直连**，
        // 不会偷偷回落到环境变量里的那个死代理。
        _ => builder.proxy(reqwest::Proxy::custom(move |url| {
            let plan = resolver();
            match plan {
                ProxyPlan::Explicit(proxy) => proxy
                    .proxy_for(url.scheme(), url.host_str().unwrap_or_default())
                    .map(str::to_string),
                _ => None,
            }
        })),
    }
}

/// 第一次建客户端时打一行"它到底走了哪条路"。
///
/// 为什么值得占一行日志：出网问题里最先要回答的就是这个问题，而它**没有别的出口**
/// （界面只显示余额/任务，不会显示代理；用户也不会想到去看注册表）。
fn log_once() {
    static LOGGED: OnceLock<()> = OnceLock::new();
    LOGGED.get_or_init(|| {
        crate::v2::logging::log_line(&format!(
            "出网代理：{}",
            crate::v2::outbound::describe()
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::outbound::{ExplicitProxy, ProxySource};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    /// 一个"只会说 204"的假代理。它**收到的那行请求**就是"代理到底有没有被用上"的证据。
    ///
    /// 用 std 的阻塞 socket + 独立线程：不起 tokio 的 io 扩展特性，测试也不占 runtime。
    /// 等待窗口 1.5 秒——**没被用上**的那条判据需要一个"等过一小会儿仍然没人来"的观察期。
    fn fake_proxy() -> (String, std::thread::JoinHandle<Option<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("占一个端口");
        let address = listener.local_addr().expect("读出端口");
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).ok()?;
            let deadline = Instant::now() + Duration::from_millis(1500);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok()?;
                        let mut buffer = [0u8; 4096];
                        let read = stream.read(&mut buffer).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                        let _ = stream
                            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
                        return Some(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return None;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return None,
                }
            }
        });
        (format!("http://{address}"), handle)
    }

    fn proxy_for(source: ProxySource, url: String) -> ExplicitProxy {
        let mut explicit = ExplicitProxy::new(source);
        explicit.all = Some(url);
        explicit
    }

    fn plan_with(source: ProxySource, url: String) -> ProxyPlan {
        ProxyPlan::Explicit(proxy_for(source, url))
    }

    /// **代理真的被用上了**（正向），而且**关掉之后真的不会再用**（负对照）。
    ///
    /// 判据用"代理收到的那行请求"：走正向代理的 HTTP 请求是**绝对形式** URI
    /// （`GET http://example.invalid/probe HTTP/1.1`），这与直连的 `/probe` 一眼可分。
    #[tokio::test]
    async fn an_explicit_proxy_is_used_and_off_really_means_off() {
        // ① 装了代理 ⇒ 请求发给代理。
        let (proxy_url, handle) = fake_proxy();
        let plan = plan_with(ProxySource::System, proxy_url);
        let resolver = plan.clone();
        let client = apply_with(reqwest::Client::builder(), &plan, move || resolver.clone())
            .build()
            .expect("建客户端");
        let response = client
            .get("http://example.invalid/probe")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("应当经由代理拿到响应");
        assert_eq!(response.status().as_u16(), 204);
        let request = handle.join().expect("代理线程").expect("代理应当收到连接");
        assert!(
            request.starts_with("GET http://example.invalid/probe"),
            "绝对形式的 URI 说明它发给了代理，实际：{request}"
        );

        // ② 明确关掉 ⇒ 同一个地址不会打到代理（`example.invalid` 按 RFC 2606 不该被解析）。
        let (_proxy_url, handle) = fake_proxy();
        let client = apply(
            reqwest::Client::builder(),
            &ProxyPlan::Off { disabled: true },
        )
        .build()
        .expect("建客户端");
        let _ = client
            .get("http://example.invalid/probe")
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        assert!(
            handle.join().expect("代理线程").is_none(),
            "关掉代理之后不该有任何连接打到代理"
        );
    }

    /// **系统代理是活的**：用户在应用启动之后才打开代理软件，下一次请求就该跟上，
    /// 不需要重启、也不需要重建客户端。
    ///
    /// 这条是"跟随系统"是否名副其实的唯一判据。用同一行 `client` 发两次请求、中间只翻
    /// resolver 的返回值，代理那头看到的**只能是第二次那条**（`/after`）——
    /// 如果第一次就打了 `/before`，说明闭包在第一次请求时就把结果记死了。
    ///
    /// 负对照：把 `Proxy::custom` 换成"建客户端时算一次"（静态 `apply` 的旧写法）⇒ 第二次
    /// 仍然是直连，代理收不到任何东西。
    #[tokio::test]
    async fn a_proxy_that_appears_later_is_picked_up_without_rebuilding() {
        let (proxy_url, handle) = fake_proxy();
        let current = std::sync::Arc::new(std::sync::Mutex::new(ProxyPlan::Off {
            disabled: false,
        }));
        let resolver = {
            let current = current.clone();
            move || current.lock().unwrap().clone()
        };
        let client = apply_with(
            reqwest::Client::builder(),
            &ProxyPlan::Off { disabled: false },
            resolver,
        )
        .build()
        .expect("建客户端");

        // ① 代理还没开：这次请求走直连（`example.invalid` 解析不了，失败也算过关）。
        let _ = client
            .get("http://example.invalid/before")
            .timeout(Duration::from_secs(2))
            .send()
            .await;

        // ② 用户这时候打开了代理软件（系统设置变了）。
        *current.lock().unwrap() = plan_with(ProxySource::System, proxy_url);

        // ③ 同一个客户端，下一次请求就该经代理。
        let response = client
            .get("http://example.invalid/after")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("这一次应当经由代理");
        assert_eq!(response.status().as_u16(), 204);

        let request = handle.join().expect("代理线程").expect("代理应当收到连接");
        assert!(
            request.starts_with("GET http://example.invalid/after"),
            "代理只该收到『开代理之后』那一条（收到 /before 说明它把结果记死了）：{request}"
        );
    }

    /// Live smoke：**系统代理真的能把流量带出去**。
    ///
    /// 判据是"直连不通、走代理通"——只有这种组合才能证明我们**真的用上了**那份配置；
    /// 拿一个本来就能直连的地址（比如很多机器上的 runninghub.cn）测，什么都证明不了。
    ///
    /// 跑法：`cargo test --lib -- --ignored live_system_proxy_carries_the_request --nocapture`
    /// 前提：系统代理开着，且 `https://www.google.com` 直连不通。不满足就 SKIP。
    #[tokio::test]
    #[ignore]
    async fn live_system_proxy_carries_the_request() {
        const PROXIED_ONLY: &str = "https://www.google.com";

        // ① 直连：这一步**必须失败**，否则这条判据无从谈起。
        let direct = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .expect("建客户端")
            .get(PROXIED_ONLY)
            .send()
            .await;
        if direct.is_ok() {
            eprintln!("SKIP: 直连 {PROXIED_ONLY} 就能通，判据不成立");
            return;
        }

        // ② 同一台机器、同一个地址，换成"我们装配出来的客户端"。
        let client = configure(reqwest::Client::builder())
            .timeout(Duration::from_secs(12))
            .build()
            .expect("建客户端");
        let response = client
            .get(PROXIED_ONLY)
            .send()
            .await
            .expect("系统代理应当能把它带出去");
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "状态码 {} 不对",
            response.status()
        );
        eprintln!(
            "✓ 直连失败、经系统代理成功（{}）：{}",
            response.status(),
            crate::v2::outbound::describe()
        );
    }

    /// **回环地址永远直连**：本机 ComfyUI 不能因为机器上开着 Clash 就变成"连不上"。
    #[tokio::test]
    async fn a_loopback_target_never_goes_through_the_proxy() {
        let target = std::net::TcpListener::bind("127.0.0.1:0").expect("占端口");
        let target_address = target.local_addr().expect("读出端口");
        let target_task = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = target.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let (proxy_url, proxy_handle) = fake_proxy();

        let plan = plan_with(ProxySource::System, proxy_url);
        let resolver = plan.clone();
        let client = apply_with(reqwest::Client::builder(), &plan, move || resolver.clone())
            .build()
            .expect("建客户端");
        let response = client
            .get(format!("http://{target_address}/probe"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("回环地址应当直连成功");
        assert_eq!(response.status().as_u16(), 204);
        assert!(
            proxy_handle.join().expect("代理线程").is_none(),
            "回环地址不该走代理——走了的话本机 ComfyUI 会跟着挂"
        );
        let _ = target_task.join();
    }
}
