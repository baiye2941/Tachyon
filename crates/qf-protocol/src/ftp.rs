//! FTP 客户端实现
//!
//! 基于 tokio::net::TcpStream 的异步 FTP 客户端,支持:
//! - 主动/被动模式连接
//! - 用户名/密码认证
//! - 文件大小查询
//! - 文件下载
//!
//! 当前为骨架实现,核心方法返回 Protocol 错误,
//! 后续完成 FTP 协议状态机后启用端到端逻辑。

use bytes::Bytes;
use qf_core::traits::Protocol;
use qf_core::types::FileMetadata;
use qf_core::{QfError, QfResult};

/// FTP 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FtpState {
    /// 未连接
    Disconnected,
    /// 已建立 TCP 连接,等待登录
    Connected,
    /// 已登录,可以执行命令
    Authenticated,
}

/// FTP 协议客户端
///
/// 管理 FTP 控制连接的生命周期和认证状态。
pub struct FtpClient {
    /// 当前连接状态
    state: FtpState,
    /// 远程主机地址
    host: Option<String>,
    /// 远程端口
    port: Option<u16>,
    /// 登录用户名
    user: Option<String>,
}

impl FtpClient {
    /// 创建新的 FTP 客户端实例
    pub fn new() -> Self {
        Self {
            state: FtpState::Disconnected,
            host: None,
            port: None,
            user: None,
        }
    }

    /// 连接到 FTP 服务器
    ///
    /// 建立 TCP 控制连接并读取服务器欢迎消息。
    pub async fn connect(&mut self, host: &str, port: u16) -> QfResult<()> {
        // 后续实现:tokio::net::TcpStream::connect + 读取 220 欢迎消息
        self.host = Some(host.to_string());
        self.port = Some(port);
        self.state = FtpState::Connected;
        tracing::info!(host, port, "FTP 控制连接已建立");
        Ok(())
    }

    /// 使用 FTP 登录
    ///
    /// 发送 USER 和 PASS 命令完成认证。
    pub async fn login(&mut self, user: &str, pass: &str) -> QfResult<()> {
        if self.state != FtpState::Connected {
            return Err(QfError::Protocol(
                "FTP 未连接,无法登录 -- 请先调用 connect()".into(),
            ));
        }
        // 后续实现:发送 USER <user>\r\n 和 PASS <pass>\r\n
        self.user = Some(user.to_string());
        self.state = FtpState::Authenticated;
        tracing::info!(user, "FTP 登录成功");
        let _ = pass; // pass 当前未存储,后续发送给服务端
        Ok(())
    }

    /// 查询远程文件大小
    ///
    /// 发送 SIZE 命令获取文件字节数。
    pub async fn file_size(&mut self, path: &str) -> QfResult<u64> {
        self.require_authenticated()?;
        // 后续实现:发送 SIZE <path>\r\n 并解析 213 响应
        tracing::warn!(path, "FTP file_size 尚未完全实现");
        Err(QfError::Protocol("FTP 传输尚未完全实现".into()))
    }

    /// 下载整个文件
    ///
    /// 使用 RETR 命令通过数据连接接收文件内容。
    pub async fn retrieve(&mut self, path: &str) -> QfResult<Bytes> {
        self.require_authenticated()?;
        // 后续实现:发送 PASV + RETR <path>\r\n 并读取数据连接
        tracing::warn!(path, "FTP retrieve 尚未完全实现");
        Err(QfError::Protocol("FTP 传输尚未完全实现".into()))
    }

    /// 当前是否已连接(包含已认证状态)
    pub fn is_connected(&self) -> bool {
        matches!(self.state, FtpState::Connected | FtpState::Authenticated)
    }

    /// 当前是否已登录
    pub fn is_authenticated(&self) -> bool {
        self.state == FtpState::Authenticated
    }

    /// 获取远程主机地址
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// 获取远程端口
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// 获取登录用户名
    pub fn username(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// 断开连接
    pub fn disconnect(&mut self) {
        self.state = FtpState::Disconnected;
        self.host = None;
        self.port = None;
        self.user = None;
    }

    /// 检查是否已认证,未认证则返回错误
    fn require_authenticated(&self) -> QfResult<()> {
        if self.state != FtpState::Authenticated {
            return Err(QfError::Protocol(
                "FTP 未登录,无法执行命令 -- 请先调用 login()".into(),
            ));
        }
        Ok(())
    }
}

impl Default for FtpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Protocol for FtpClient {
    async fn probe(&self, _url: &str) -> QfResult<FileMetadata> {
        // FTP URL 格式: ftp://user:pass@host:port/path
        // 后续实现:解析 URL -> 连接 -> 登录 -> SIZE/MLST
        Err(QfError::Protocol("FTP 传输尚未完全实现".into()))
    }

    async fn download_range(&self, _url: &str, _start: u64, _end: u64) -> QfResult<Bytes> {
        // FTP Range 下载需要 REST 命令支持
        Err(QfError::Protocol("FTP 传输尚未完全实现".into()))
    }

    async fn download_full(&self, _url: &str) -> QfResult<Bytes> {
        Err(QfError::Protocol("FTP 传输尚未完全实现".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ftp_client_creation() {
        let client = FtpClient::new();
        assert!(!client.is_connected(), "新客户端不应处于已连接状态");
        assert!(!client.is_authenticated(), "新客户端不应处于已认证状态");
        assert!(client.host().is_none());
        assert!(client.port().is_none());
        assert!(client.username().is_none());
    }

    #[test]
    fn test_ftp_client_default() {
        let client = FtpClient::default();
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn test_ftp_connect_sets_state() {
        let mut client = FtpClient::new();
        client.connect("ftp.example.com", 21).await.unwrap();
        assert!(client.is_connected());
        assert!(!client.is_authenticated());
        assert_eq!(client.host(), Some("ftp.example.com"));
        assert_eq!(client.port(), Some(21));
    }

    #[tokio::test]
    async fn test_ftp_login_after_connect() {
        let mut client = FtpClient::new();
        client.connect("ftp.example.com", 21).await.unwrap();
        client.login("anonymous", "guest@").await.unwrap();
        assert!(client.is_authenticated());
        assert_eq!(client.username(), Some("anonymous"));
    }

    #[tokio::test]
    async fn test_ftp_login_without_connect_fails() {
        let mut client = FtpClient::new();
        let result = client.login("user", "pass").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("FTP 未连接"));
    }

    #[tokio::test]
    async fn test_ftp_file_size_without_auth_fails() {
        let mut client = FtpClient::new();
        client.connect("ftp.example.com", 21).await.unwrap();
        let result = client.file_size("/path/file.zip").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("FTP 未登录"));
    }

    #[tokio::test]
    async fn test_ftp_retrieve_without_auth_fails() {
        let mut client = FtpClient::new();
        client.connect("ftp.example.com", 21).await.unwrap();
        let result = client.retrieve("/path/file.zip").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ftp_probe_returns_not_implemented() {
        let client = FtpClient::new();
        let result = client.probe("ftp://example.com/file.zip").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("FTP 传输尚未完全实现")
        );
    }

    #[tokio::test]
    async fn test_ftp_download_range_returns_not_implemented() {
        let client = FtpClient::new();
        let result = client
            .download_range("ftp://example.com/file.zip", 0, 1023)
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("FTP 传输尚未完全实现")
        );
    }

    #[tokio::test]
    async fn test_ftp_download_full_returns_not_implemented() {
        let client = FtpClient::new();
        let result = client.download_full("ftp://example.com/file.zip").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("FTP 传输尚未完全实现")
        );
    }

    #[tokio::test]
    async fn test_ftp_disconnect_resets_state() {
        let mut client = FtpClient::new();
        client.connect("ftp.example.com", 21).await.unwrap();
        client.login("user", "pass").await.unwrap();
        assert!(client.is_authenticated());

        client.disconnect();
        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
        assert!(client.host().is_none());
    }

    #[tokio::test]
    async fn test_ftp_state_transitions() {
        let mut client = FtpClient::new();

        // Disconnected -> Connected
        assert!(!client.is_connected());
        client.connect("ftp.example.com", 21).await.unwrap();
        assert!(client.is_connected());
        assert!(!client.is_authenticated());

        // Connected -> Authenticated
        client.login("admin", "secret").await.unwrap();
        assert!(client.is_connected());
        assert!(client.is_authenticated());

        // Authenticated -> Disconnected
        client.disconnect();
        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
    }
}
