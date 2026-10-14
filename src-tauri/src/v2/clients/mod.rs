pub mod cli;
pub mod client;
/// 客户端自己把 daemon 拉起来（**启动，但不拥有**）。
pub mod daemon_boot;
pub mod mcp;
/// 动词表：一份定义（动词 + 参数 + 通道），CLI 与 MCP 各自渲染。
pub mod verbs;
