pub mod application;
pub mod clients;
pub mod daemon;
pub mod domain;
/// 进程的**环境事实**（绑定地址 / host 白名单 / 界面来源 / 静态目录）。
///
/// 单独成模块的理由与 `outbound` 一样：**daemon 与 transports 都要用同一批值**
/// （诊断接口要报的正是服务实际生效的那些），而 transports 不得 import daemon。
pub mod environment;
pub mod infrastructure;
pub mod logging;
pub mod outbound;
pub mod ports;
pub mod transports;
