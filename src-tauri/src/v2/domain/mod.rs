pub mod actor;
pub mod callback;
/// 连接的**可选运行参数**（请求超时 / 对账死线）：只有用户环境才知道的那几个量。
///
/// 放 domain 的理由与 `signing` 同类但方向相反：它要**被 infrastructure 的 driver 读**
/// （超时在 driver 里用），而 driver 不得 import application。
pub mod connection_options;
pub mod errors;
pub mod event;
pub mod execution;
pub mod hash;
pub mod ids;
pub mod media;
pub mod model;
pub mod operation;
/// 往外写的文字里 URL 只留 scheme + host（密钥可能被拼在 URL 上）。
///
/// 放 domain：错误详情在 infrastructure、日志在 `v2::logging`，两边必须用同一条规则。
pub mod redact;
/// 回调签名：与接收端共享的**契约**（HMAC-SHA256，RFC 4231 向量钉住）。
///
/// 放在 domain 而不是 infrastructure：它是纯计算、无 IO，而且 **application 层要用它**
/// （application 不得 import infrastructure，有分层守门）。
pub mod signing;
