//! FTP 客户端实现
//!
//! 基于 suppaftp crate 的异步 FTP 客户端,支持:
//! - 主动/被动模式连接
//! - 用户名/密码认证(匿名登录)
//! - 文件大小查询(SIZE 命令)
//! - 文件下载(RETR 命令)
//! - Binary 传输模式
//!
//! `Protocol` trait 方法(`probe`/`download_range`/`download_full`)每次调用
//! 均建立独立连接,操作完成后自动断开,适合无状态调用场景。
//! `connect`/`login`/`retrieve` 等实例方法维护持久连接状态。

use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use amd_core::traits::Protocol;
use amd_core::types::FileMetadata;
use amd_core::url_safety::reject_forbidden_ip;
use amd_core::{AmdError, AmdResult, ByteStream};
use bytes::Bytes;
use suppaftp::tokio::AsyncFtpStream;
use suppaftp::types::FileType;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

/// FTP 连接状态
///
/// 跟踪控制连接的生命周期阶段:
/// - `Disconnected`: 未建立 TCP 连接
/// - `Connected`: 已建立 TCP 连接,等待 USER/PASS 认证
/// - `Authenticated`: 已登录,可执行 FTP 命令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FtpState {
    /// 未连接
    Disconnected,
    /// 已建立 TCP 连接,等待登录
    Connected,
    /// 已登录,可以执行命令
    Authenticated,
}

/// 内部连接状态:绑定 FTP 流和认证信息
struct ConnectionState {
    /// 当前连接状态
    state: FtpState,
    /// suppaftp 异步 FTP 流(含控制连接)
    stream: Option<AsyncFtpStream>,
    /// 远程主机地址
    host: String,
    /// 远程端口
    port: u16,
    /// 登录用户名(匿名登录时为 "anonymous")
    username: Option<String>,
}

/// FTP 协议客户端
///
/// 内部使用 `Arc<RwLock>` 保护连接状态,读操作使用共享读锁,
/// 写操作使用排他写锁。`Clone` 实现共享同一底层连接状态。
#[derive(Clone)]
pub struct FtpClient {
    /// 持久连接状态(通过 Arc<RwLock> 实现读写分离与共享)
    inner: Arc<RwLock<ConnectionState>>,
}

impl FtpClient {
    /// 创建新的 FTP 客户端实例
    ///
    /// 初始状态为 `Disconnected`,不建立任何网络连接。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ConnectionState {
                state: FtpState::Disconnected,
                stream: None,
                host: String::new(),
                port: 0,
                username: None,
            })),
        }
    }

    /// 连接到 FTP 服务器
    ///
    /// 建立 TCP 控制连接并读取服务器 220 欢迎消息。
    /// 地址格式: `host:port`(如 `"ftp.example.com:21"`)。
    pub async fn connect(&self, host: &str, port: u16) -> AmdResult<()> {
        let normalized_host = host.trim_end_matches('.');
        if normalized_host.eq_ignore_ascii_case("localhost") {
            return Err(AmdError::Protocol("FTP 不允许访问 localhost".into()));
        }
        // IPv6 地址可能带方括号(如 "[::1]"),需先去除再解析
        let stripped = normalized_host
            .trim_start_matches('[')
            .trim_end_matches(']');
        if let Ok(ip) = stripped.parse::<IpAddr>() {
            reject_forbidden_ip(ip).map_err(|e| AmdError::Protocol(e.to_string()))?;
        }

        let addr = format!("{host}:{port}");
        let stream = AsyncFtpStream::connect(&addr)
            .await
            .map_err(|e| AmdError::Network(format!("FTP 连接失败: {e}")))?;

        let mut guard = self.inner.write().await;
        guard.stream = Some(stream);
        guard.host = host.to_string();
        guard.port = port;
        guard.state = FtpState::Connected;
        tracing::info!(host, port, "FTP 控制连接已建立");
        Ok(())
    }

    /// 使用用户名和密码登录 FTP 服务器
    ///
    /// 发送 USER 和 PASS 命令完成认证。
    /// 必须在 `connect()` 成功后调用。
    pub async fn login(&self, user: &str, pass: &str) -> AmdResult<()> {
        let mut guard = self.inner.write().await;
        if guard.state != FtpState::Connected {
            return Err(AmdError::Protocol(
                "FTP 未连接,无法登录 -- 请先调用 connect()".into(),
            ));
        }
        let stream = guard
            .stream
            .as_mut()
            .ok_or_else(|| AmdError::Protocol("FTP 流不可用".into()))?;

        stream
            .login(user, pass)
            .await
            .map_err(|e| AmdError::Protocol(format!("FTP 登录失败: {e}")))?;

        guard.username = Some(user.to_string());
        guard.state = FtpState::Authenticated;
        tracing::info!(user, "FTP 登录成功");
        Ok(())
    }

    /// 查询远程文件大小(字节)
    ///
    /// 发送 SIZE 命令获取指定路径文件的字节数。
    /// 必须在 `login()` 成功后调用。
    pub async fn file_size(&self, path: &str) -> AmdResult<u64> {
        let mut guard = self.inner.write().await;
        Self::require_stream(&guard)?;
        let stream = guard.stream.as_mut().expect("require_stream 已验证流存在");

        let size = stream
            .size(path)
            .await
            .map_err(|e| AmdError::Protocol(format!("获取文件大小失败: {e}")))?;
        Ok(size as u64)
    }

    /// 下载整个文件到内存
    ///
    /// 使用 RETR 命令通过数据连接接收文件内容。
    /// FTP 不原生支持 Range 请求,此方法始终下载完整文件。
    /// 必须在 `login()` 成功后调用。
    pub async fn retrieve(&self, path: &str) -> AmdResult<Bytes> {
        let mut guard = self.inner.write().await;
        Self::require_stream(&guard)?;
        let stream = guard.stream.as_mut().expect("require_stream 已验证流存在");

        // 确保使用 Binary 模式,避免文本文件换行符转换导致数据损坏
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| AmdError::Protocol(format!("设置传输模式失败: {e}")))?;

        // 获取数据流并读取全部内容
        let mut data_stream = stream
            .retr_as_stream(path)
            .await
            .map_err(|e| AmdError::Protocol(format!("下载文件失败: {e}")))?;

        let mut buf = Vec::new();
        data_stream
            .read_to_end(&mut buf)
            .await
            .map_err(|e| AmdError::Network(format!("读取 FTP 数据流失败: {e}")))?;

        // 完成 RETR 传输并读取服务端响应
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| AmdError::Protocol(format!("完成 FTP 传输失败: {e}")))?;

        Ok(Bytes::from(buf))
    }

    /// 使用 REST 命令从指定偏移处恢复下载,并截取所需字节范围
    ///
    /// FTP 的 REST 命令设置文件指针偏移,然后 RETR 从该偏移开始传输。
    /// 我们从 `start` 偏移处开始下载,读取 `end - start + 1` 字节后关闭数据连接。
    /// 必须在 `login()` 成功后调用。
    pub async fn retrieve_range(&self, path: &str, start: u64, end: u64) -> AmdResult<Bytes> {
        let mut guard = self.inner.write().await;
        Self::require_stream(&guard)?;
        let stream = guard
            .stream
            .as_mut()
            .ok_or_else(|| AmdError::Protocol("FTP 流不可用".into()))?;

        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| AmdError::Protocol(format!("设置传输模式失败: {e}")))?;

        let need = if end >= start {
            (end - start + 1) as usize
        } else {
            return Err(AmdError::Protocol("FTP Range 无效: end < start".into()));
        };

        if start > 0 {
            stream
                .resume_transfer(start as usize)
                .await
                .map_err(|e| AmdError::Protocol(format!("REST 命令失败: {e}")))?;
        }

        let mut data_stream = stream
            .retr_as_stream(path)
            .await
            .map_err(|e| AmdError::Protocol(format!("下载文件失败: {e}")))?;

        let mut buf = vec![0u8; need];
        let mut total_read = 0usize;
        while total_read < need {
            let n = tokio::io::AsyncReadExt::read(&mut data_stream, &mut buf[total_read..])
                .await
                .map_err(|e| AmdError::Network(format!("读取 FTP 数据流失败: {e}")))?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        buf.truncate(total_read);

        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| AmdError::Protocol(format!("完成 FTP 传输失败: {e}")))?;

        Ok(Bytes::from(buf))
    }

    /// 当前是否已连接(包含已认证状态)
    pub async fn is_connected(&self) -> bool {
        let guard = self.inner.read().await;
        matches!(guard.state, FtpState::Connected | FtpState::Authenticated)
    }

    /// 当前是否已登录
    pub async fn is_authenticated(&self) -> bool {
        let guard = self.inner.read().await;
        guard.state == FtpState::Authenticated
    }

    /// 获取远程主机地址
    pub async fn host(&self) -> String {
        let guard = self.inner.read().await;
        guard.host.clone()
    }

    /// 获取远程端口
    pub async fn port(&self) -> u16 {
        let guard = self.inner.read().await;
        guard.port
    }

    /// 获取登录用户名
    pub async fn username(&self) -> Option<String> {
        let guard = self.inner.read().await;
        guard.username.clone()
    }

    /// 断开连接并释放资源
    ///
    /// 发送 QUIT 命令优雅关闭控制连接,重置所有内部状态。
    pub async fn disconnect(&self) {
        let mut guard = self.inner.write().await;
        if let Some(mut stream) = guard.stream.take() {
            let _ = stream.quit().await;
        }
        guard.state = FtpState::Disconnected;
        guard.host = String::new();
        guard.port = 0;
        guard.username = None;
    }

    // --------------- 内部辅助方法 ---------------

    /// 检查 FTP 流是否可用(已连接且流存在)
    fn require_stream(state: &ConnectionState) -> AmdResult<()> {
        if state.stream.is_none() {
            return Err(AmdError::Protocol(
                "FTP 未连接,无法执行命令 -- 请先调用 connect()".into(),
            ));
        }
        Ok(())
    }

    /// 从 FTP URL 解析连接信息
    ///
    /// URL 格式: `ftp://[user[:password]@]host[:port]/path`
    ///
    /// - 用户名默认 `"anonymous"`
    /// - 密码默认 `"anonymous@aimodeldownloader"`
    /// - 端口默认 `21`
    fn parse_ftp_url(url: &str) -> AmdResult<FtpUrl> {
        let parsed = url::Url::parse(url)
            .map_err(|e| AmdError::Protocol(format!("FTP URL 解析失败: {e}")))?;

        if parsed.scheme() != "ftp" {
            return Err(AmdError::Protocol(format!(
                "URL 方案不是 ftp: {}",
                parsed.scheme()
            )));
        }

        // SSRF 防护:直接使用 url crate 解析后的 Host 枚举,
        // 避免 host_str() 对 IPv6 返回带方括号的字符串导致解析失败
        match parsed.host() {
            Some(url::Host::Ipv4(v4)) => {
                reject_forbidden_ip(IpAddr::V4(v4))
                    .map_err(|e| AmdError::Protocol(e.to_string()))?;
            }
            Some(url::Host::Ipv6(v6)) => {
                reject_forbidden_ip(IpAddr::V6(v6))
                    .map_err(|e| AmdError::Protocol(e.to_string()))?;
            }
            Some(url::Host::Domain(domain)) => {
                let normalized = domain.trim_end_matches('.');
                if normalized.eq_ignore_ascii_case("localhost") {
                    return Err(AmdError::Protocol("FTP 不允许访问 localhost".into()));
                }
                // 域名可能解析为受限 IP,但此处在 URL 解析阶段无法验证,
                // 需在 DNS 解析阶段(如 connect)再次检查
            }
            None => {
                return Err(AmdError::Protocol("FTP URL 缺少主机名".into()));
            }
        }

        let host = parsed
            .host_str()
            .ok_or_else(|| AmdError::Protocol("FTP URL 缺少主机名".into()))?
            .to_string();

        let port = parsed.port().unwrap_or(21);

        let username = if parsed.username().is_empty() {
            "anonymous".to_string()
        } else {
            parsed.username().to_string()
        };

        let password = parsed
            .password()
            .unwrap_or("anonymous@aimodeldownloader")
            .to_string();

        let path = parsed.path().to_string();

        Ok(FtpUrl {
            host,
            port,
            username,
            password,
            path,
        })
    }

    /// 从路径中提取文件名
    ///
    /// - `"/pub/file.zip"` -> `"file.zip"`
    /// - `"/"` -> `"download"`
    /// - `"file.txt"` -> `"file.txt"`
    fn extract_filename_from_path(path: &str) -> String {
        path.rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("download")
            .to_string()
    }
}

impl Default for FtpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// FTP URL 解析结果
impl std::fmt::Debug for FtpUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpUrl")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("path", &self.path)
            .finish()
    }
}

struct FtpUrl {
    host: String,
    port: u16,
    username: String,
    password: String,
    path: String,
}

impl Protocol for FtpClient {
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<FileMetadata>> + Send>> {
        let url = url.to_owned();
        let this = self.clone();
        Box::pin(async move {
            let info = Self::parse_ftp_url(&url)?;

            this.connect(&info.host, info.port).await?;

            if let Err(e) = this.login(&info.username, &info.password).await {
                this.disconnect().await;
                return Err(e);
            }

            let size_result = this.file_size(&info.path).await;

            this.disconnect().await;

            let file_size = Some(size_result?);

            Ok(FileMetadata {
                file_name: Self::extract_filename_from_path(&info.path),
                file_size,
                content_type: None,
                supports_range: false,
                etag: None,
                last_modified: None,
            })
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
        let url = url.to_owned();
        let this = self.clone();
        Box::pin(async move {
            if start == 0 && end == 0 {
                return this.download_full(&url).await;
            }

            let info = Self::parse_ftp_url(&url)?;

            this.connect(&info.host, info.port).await?;
            if let Err(e) = this.login(&info.username, &info.password).await {
                this.disconnect().await;
                return Err(e);
            }

            let result = this.retrieve_range(&info.path, start, end).await;
            this.disconnect().await;
            result
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<ByteStream>> + Send>> {
        let url = url.to_owned();
        let this = self.clone();
        // TODO: FTP 实现仍为整块缓冲,后续改为按 chunk 流式读取 FTP 数据连接
        Box::pin(async move {
            let data = this.download_range(&url, start, end).await?;
            Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = AmdResult<Bytes>> + Send>> {
        let url = url.to_owned();
        let this = self.clone();
        Box::pin(async move {
            let info = Self::parse_ftp_url(&url)?;

            this.connect(&info.host, info.port).await?;

            if let Err(e) = this.login(&info.username, &info.password).await {
                this.disconnect().await;
                return Err(e);
            }

            let result = this.retrieve(&info.path).await;

            this.disconnect().await;

            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // 辅助方法:允许测试直接设置内部状态,无需实际网络连接
    // =========================================================================

    impl FtpClient {
        /// [仅测试] 直接将状态设为 Connected,跳过真实 TCP 连接
        async fn set_connected_for_test(&self, host: &str, port: u16) {
            let mut guard = self.inner.write().await;
            guard.state = FtpState::Connected;
            guard.host = host.to_string();
            guard.port = port;
        }

        /// [仅测试] 直接将状态设为 Authenticated,跳过真实连接和登录
        async fn set_authenticated_for_test(&self, host: &str, port: u16, user: &str) {
            let mut guard = self.inner.write().await;
            guard.state = FtpState::Authenticated;
            guard.host = host.to_string();
            guard.port = port;
            guard.username = Some(user.to_string());
        }

        /// [仅测试] 直接将状态设为 Disconnected,重置所有字段
        async fn set_disconnected_for_test(&self) {
            let mut guard = self.inner.write().await;
            guard.state = FtpState::Disconnected;
            guard.stream = None;
            guard.host = String::new();
            guard.port = 0;
            guard.username = None;
        }
    }

    // =========================================================================
    // 1. 客户端创建与初始状态
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_client_creation() {
        let client = FtpClient::new();
        assert!(!client.is_connected().await, "新客户端不应处于已连接状态");
        assert!(
            !client.is_authenticated().await,
            "新客户端不应处于已认证状态"
        );
        assert_eq!(client.host().await, "");
        assert_eq!(client.port().await, 0);
        assert!(client.username().await.is_none());
    }

    #[tokio::test]
    async fn test_ftp_client_default() {
        let client = FtpClient::default();
        assert!(!client.is_connected().await);
        assert_eq!(client.host().await, "");
    }

    // =========================================================================
    // 2. 未连接/未认证时调用方法返回正确错误
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_login_without_connect_fails() {
        let client = FtpClient::new();
        let result = client.login("user", "pass").await;
        assert!(result.is_err(), "未连接时登录应失败");

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("FTP 未连接"),
            "错误应提示未连接状态"
        );
    }

    #[tokio::test]
    async fn test_ftp_file_size_without_stream_fails() {
        let client = FtpClient::new();
        // 设置为 Authenticated 但无 FtpStream
        client
            .set_authenticated_for_test("ftp.test.com", 21, "user")
            .await;

        let result = client.file_size("/file.zip").await;
        assert!(result.is_err(), "无流时 file_size 应失败");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FTP 未连接"),
            "错误应包含 FTP 未连接提示,实际: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_ftp_retrieve_without_stream_fails() {
        let client = FtpClient::new();
        client
            .set_authenticated_for_test("ftp.test.com", 21, "user")
            .await;

        let result = client.retrieve("/file.zip").await;
        assert!(result.is_err(), "无流时 retrieve 应失败");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FTP 未连接"),
            "错误应包含 FTP 未连接提示,实际: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_ftp_login_on_disconnected_state_fails() {
        let client = FtpClient::new();
        client.set_disconnected_for_test().await;

        let result = client.login("user", "pass").await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("FTP 未连接"),
            "Disconnected 状态登录应返回未连接错误"
        );
    }

    // =========================================================================
    // 3. 状态转换逻辑
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_state_transitions() {
        let client = FtpClient::new();

        // 初始: Disconnected
        assert!(!client.is_connected().await);
        assert!(!client.is_authenticated().await);

        // Disconnected -> Connected
        client.set_connected_for_test("ftp.example.com", 21).await;
        assert!(client.is_connected().await);
        assert!(!client.is_authenticated().await);

        // Connected -> Authenticated
        client
            .set_authenticated_for_test("ftp.example.com", 21, "admin")
            .await;
        assert!(client.is_connected().await);
        assert!(client.is_authenticated().await);

        // Authenticated -> Disconnected
        client.set_disconnected_for_test().await;
        assert!(!client.is_connected().await);
        assert!(!client.is_authenticated().await);
    }

    #[tokio::test]
    async fn test_ftp_disconnect_resets_all_fields() {
        let client = FtpClient::new();
        client
            .set_authenticated_for_test("ftp.example.com", 21, "admin")
            .await;

        assert_eq!(client.host().await, "ftp.example.com");
        assert_eq!(client.port().await, 21);
        assert_eq!(client.username().await.as_deref(), Some("admin"));

        // disconnect() 需要真实流才能发送 QUIT,这里直接用 set_disconnected_for_test
        client.set_disconnected_for_test().await;

        assert_eq!(client.host().await, "");
        assert_eq!(client.port().await, 0);
        assert!(client.username().await.is_none());
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn test_ftp_connect_sets_host_and_port() {
        let client = FtpClient::new();
        client.set_connected_for_test("ftp.test.com", 2121).await;

        assert_eq!(client.host().await, "ftp.test.com");
        assert_eq!(client.port().await, 2121);
        assert!(client.is_connected().await);
        assert!(!client.is_authenticated().await);
    }

    // =========================================================================
    // 4. FTP URL 解析
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_url_parsing_full() {
        let info = FtpClient::parse_ftp_url("ftp://admin:secret@ftp.example.com:2121/pub/file.zip")
            .unwrap();
        assert_eq!(info.host, "ftp.example.com");
        assert_eq!(info.port, 2121);
        assert_eq!(info.username, "admin");
        assert_eq!(info.password, "secret");
        assert_eq!(info.path, "/pub/file.zip");
    }

    #[tokio::test]
    async fn test_ftp_url_parsing_defaults() {
        let info = FtpClient::parse_ftp_url("ftp://ftp.example.com/path/file.txt").unwrap();
        assert_eq!(info.host, "ftp.example.com");
        assert_eq!(info.port, 21, "未指定端口应默认为 21");
        assert_eq!(info.username, "anonymous", "未指定用户名应默认为 anonymous");
        assert_eq!(
            info.password, "anonymous@aimodeldownloader",
            "未指定密码应使用默认值"
        );
        assert_eq!(info.path, "/path/file.txt");
    }

    #[tokio::test]
    async fn test_ftp_url_parsing_with_user_no_password() {
        let info = FtpClient::parse_ftp_url("ftp://myuser@ftp.example.com/file.bin").unwrap();
        assert_eq!(info.username, "myuser");
        assert_eq!(
            info.password, "anonymous@aimodeldownloader",
            "有用户名但无密码时应使用默认密码"
        );
    }

    #[tokio::test]
    async fn test_ftp_url_parsing_root_path() {
        let info = FtpClient::parse_ftp_url("ftp://ftp.example.com/").unwrap();
        assert_eq!(info.path, "/");
    }

    #[tokio::test]
    async fn test_ftp_url_parse_invalid_scheme() {
        let result = FtpClient::parse_ftp_url("http://example.com/file.zip");
        assert!(result.is_err(), "非 ftp:// 方案应返回错误");
        assert!(
            result.unwrap_err().to_string().contains("URL 方案不是 ftp"),
            "错误应提示方案不正确"
        );
    }

    #[tokio::test]
    async fn test_ftp_url_parse_no_host() {
        // ftp:// 无主机名时,url crate 返回 EmptyHost 错误
        let result = FtpClient::parse_ftp_url("ftp://");
        assert!(result.is_err(), "缺少主机名应返回错误");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("FTP URL 解析失败"),
            "错误应提示 URL 解析失败,实际: {msg}"
        );
    }

    // =========================================================================
    // 4.5 SSRF 防护
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_url_rejects_loopback_ipv4() {
        let result = FtpClient::parse_ftp_url("ftp://127.0.0.1/file.zip");
        assert!(result.is_err(), "FTP 不允许访问 127.0.0.1");
        assert!(result.unwrap_err().to_string().contains("受限"));
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_cloud_metadata() {
        let result = FtpClient::parse_ftp_url("ftp://169.254.169.254/metadata");
        assert!(result.is_err(), "FTP 不允许访问云元数据端点");
        assert!(result.unwrap_err().to_string().contains("受限"));
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_private_ipv4() {
        let result = FtpClient::parse_ftp_url("ftp://10.0.0.1/file.zip");
        assert!(result.is_err(), "FTP 不允许访问私有 IPv4");
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_link_local_ipv4() {
        let result = FtpClient::parse_ftp_url("ftp://169.254.1.1/file.zip");
        assert!(result.is_err(), "FTP 不允许访问链路本地 IPv4");
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_loopback_ipv6() {
        let result = FtpClient::parse_ftp_url("ftp://[::1]/file.zip");
        assert!(result.is_err(), "FTP 不允许访问 IPv6 回环地址");
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_localhost() {
        let result = FtpClient::parse_ftp_url("ftp://localhost/file.zip");
        assert!(result.is_err(), "FTP 不允许访问 localhost");
        assert!(result.unwrap_err().to_string().contains("localhost"));
    }

    #[tokio::test]
    async fn test_ftp_url_rejects_localhost_trailing_dot() {
        let result = FtpClient::parse_ftp_url("ftp://localhost./file.zip");
        assert!(result.is_err(), "FTP 不允许访问 localhost.(尾部点号绕过)");
    }

    #[tokio::test]
    async fn test_ftp_url_accepts_public_host() {
        let result = FtpClient::parse_ftp_url("ftp://ftp.example.com/pub/file.zip");
        assert!(result.is_ok(), "公网 FTP 主机应允许访问");
    }

    // =========================================================================
    // 5. Protocol trait 一致性
    // =========================================================================

    #[tokio::test]
    async fn test_ftp_protocol_probe_returns_error_for_unreachable() {
        let client = FtpClient::new();
        let result = client.probe("ftp://192.0.2.1:1/file.zip").await;
        assert!(result.is_err(), "不可达服务器 probe 应返回错误");
    }

    #[tokio::test]
    async fn test_ftp_protocol_download_range_returns_error_for_unreachable() {
        let client = FtpClient::new();
        let result = client
            .download_range("ftp://192.0.2.1:1/file.zip", 0, 1023)
            .await;
        assert!(result.is_err(), "不可达服务器 download_range 应返回错误");
    }

    #[tokio::test]
    async fn test_ftp_protocol_download_full_returns_error_for_unreachable() {
        let client = FtpClient::new();
        let result = client.download_full("ftp://192.0.2.1:1/file.zip").await;
        assert!(result.is_err(), "不可达服务器 download_full 应返回错误");
    }

    // =========================================================================
    // 6. 文件名从路径提取
    // =========================================================================

    #[tokio::test]
    async fn test_extract_filename_from_path() {
        assert_eq!(
            FtpClient::extract_filename_from_path("/pub/archives/file.zip"),
            "file.zip"
        );
        assert_eq!(
            FtpClient::extract_filename_from_path("/file.tar.gz"),
            "file.tar.gz"
        );
        assert_eq!(
            FtpClient::extract_filename_from_path("file.txt"),
            "file.txt",
            "无斜杠路径应返回完整文件名"
        );
        assert_eq!(
            FtpClient::extract_filename_from_path("/"),
            "download",
            "空文件名应回退为 'download'"
        );
        assert_eq!(
            FtpClient::extract_filename_from_path(""),
            "download",
            "空路径应回退为 'download'"
        );
    }

    // =========================================================================
    // 7. 文件大小格式化
    // =========================================================================

    #[tokio::test]
    async fn test_file_size_formatting_in_error_message() {
        // 验证 Protocol 方法在不可达地址上生成包含有意义上下文的错误消息
        let client = FtpClient::new();
        let result = client.probe("ftp://192.0.2.1:1/large-file.bin").await;
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            !err_msg.is_empty(),
            "错误消息不应为空,应包含有用的上下文信息"
        );
    }

    // =========================================================================
    // 8. 错误消息包含有用上下文
    // =========================================================================

    #[tokio::test]
    async fn test_error_message_login_without_connect() {
        let client = FtpClient::new();
        let err = client.login("user", "pass").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("FTP 未连接"), "错误应包含状态信息: {msg}");
        assert!(msg.contains("connect()"), "错误应包含修复建议: {msg}");
    }

    #[tokio::test]
    async fn test_error_message_file_size_without_stream() {
        let client = FtpClient::new();
        client
            .set_authenticated_for_test("ftp.test.com", 21, "user")
            .await;

        let err = client.file_size("/missing.txt").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("FTP 未连接"),
            "file_size 无流错误应包含未连接提示: {msg}"
        );
    }

    #[tokio::test]
    async fn test_error_message_retrieve_without_stream() {
        let client = FtpClient::new();
        client
            .set_authenticated_for_test("ftp.test.com", 21, "user")
            .await;

        let err = client.retrieve("/missing.txt").await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("FTP 未连接"),
            "retrieve 无流错误应包含未连接提示: {msg}"
        );
    }

    #[tokio::test]
    async fn test_error_message_invalid_url_scheme() {
        let err = FtpClient::parse_ftp_url("http://example.com/file.zip").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("URL 方案不是 ftp"),
            "错误应指明方案不正确: {msg}"
        );
    }

    #[tokio::test]
    async fn test_error_message_missing_host() {
        let err = FtpClient::parse_ftp_url("ftp://").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("FTP URL 解析失败"),
            "空主机名错误应包含解析失败信息: {msg}"
        );
    }

    #[tokio::test]
    async fn test_error_message_connect_unreachable() {
        let client = FtpClient::new();
        let result = client.connect("192.0.2.1", 1).await;
        assert!(result.is_err());

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("FTP 连接失败"),
            "连接错误应包含 'FTP 连接失败' 前缀: {msg}"
        );
    }

    #[tokio::test]
    async fn test_error_message_probe_invalid_url() {
        let client = FtpClient::new();
        let result = client.probe("not-a-url").await;
        assert!(result.is_err());

        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("FTP URL 解析失败"),
            "URL 解析错误应包含上下文: {msg}"
        );
    }
}
