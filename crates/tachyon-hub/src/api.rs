//! HuggingFace Hub REST API 客户端
//!
//! 封装与 HF Hub 的 HTTP 交互, 包括文件树列表和文件下载 URL 解析。

use serde::{Deserialize, Serialize};
use tachyon_core::DownloadResult;
use tachyon_protocol::HttpClient;

use crate::lfs;
use crate::token;

/// HF Hub 文件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfFile {
    /// 文件类型: "file" | "directory"
    #[serde(rename = "type")]
    pub file_type: String,
    /// 相对路径
    pub path: String,
    /// 文件大小(字节), directory 为 0
    pub size: u64,
    /// LFS oid (仅在 LFS 文件时有值)
    pub lfs: Option<HfLfsInfo>,
}

/// LFS 对象信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfLfsInfo {
    /// LFS oid (sha256:<hex>)
    pub oid: String,
    /// 文件大小
    pub size: u64,
}

/// HuggingFace Hub API 客户端
pub struct HubApi {
    endpoint: String,
    token: Option<String>,
    http: HttpClient,
}

impl std::fmt::Debug for HubApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubApi")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

fn new_http_client() -> Result<HttpClient, tachyon_core::DownloadError> {
    HttpClient::new()
        .map_err(|e| tachyon_core::DownloadError::Config(format!("创建 Hub HTTP 客户端失败: {e}")))
}

impl HubApi {
    /// 从环境变量创建客户端
    ///
    /// 可能因 HTTP 客户端初始化失败(如 TLS 后端不可用)返回错误。
    /// endpoint 会通过 validate_public_http_url 校验,防止指向内网的 SSRF。
    pub fn from_env() -> Result<Self, tachyon_core::DownloadError> {
        let endpoint = token::hf_endpoint();
        let url: url::Url =
            url::Url::parse(&endpoint).map_err(tachyon_core::DownloadError::UrlParse)?;
        tachyon_core::validate_public_http_url(&url)?;
        Ok(Self {
            endpoint,
            token: token::load_token(),
            http: new_http_client()?,
        })
    }

    /// 使用自定义 endpoint 创建
    ///
    /// 可能因 HTTP 客户端初始化失败返回错误。
    /// endpoint 会通过 validate_public_http_url 校验,防止指向内网的 SSRF。
    pub fn with_endpoint(endpoint: String) -> Result<Self, tachyon_core::DownloadError> {
        let url: url::Url =
            url::Url::parse(&endpoint).map_err(tachyon_core::DownloadError::UrlParse)?;
        tachyon_core::validate_public_http_url(&url)?;
        Ok(Self {
            endpoint,
            token: token::load_token(),
            http: new_http_client()?,
        })
    }

    /// 获取 API 基础 URL
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// 是否有认证 Token
    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    /// 列出仓库文件树
    ///
    /// GET {endpoint}/api/models/{repo_id}/tree/{revision}?recursive=true
    pub async fn list_files(&self, repo_id: &str, revision: &str) -> DownloadResult<Vec<HfFile>> {
        let url = lfs::build_tree_url(&self.endpoint, repo_id, revision);
        tracing::info!(url = %url, "获取 HF 仓库文件树");

        let mut headers: Vec<(&str, &str)> = vec![("User-Agent", "tachyon-hub/0.1.0")];
        let auth;
        if let Some(ref token) = self.token {
            auth = format!("Bearer {token}");
            headers.push(("Authorization", &auth));
        }

        let body = self.http.get_text(&url, &headers).await?;
        let files: Vec<HfFile> =
            serde_json::from_str(&body).map_err(tachyon_core::DownloadError::Serialization)?;

        tracing::info!(count = files.len(), repo_id = %repo_id, "获取文件列表成功");
        Ok(files)
    }

    /// 为指定文件构建下载 URL
    ///
    /// 对于 LFS 文件,返回 HF Hub 的 resolve URL (HF 服务器会透明处理指针)。
    /// 对于普通文件,返回同 URL。
    pub fn download_url(&self, repo_id: &str, revision: &str, file_path: &str) -> String {
        lfs::build_resolve_url(&self.endpoint, repo_id, revision, file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M-17: with_endpoint 构造测试
    #[test]
    fn test_with_endpoint() {
        let api = HubApi::with_endpoint("https://hf-mirror.com".to_string()).unwrap();
        assert_eq!(api.endpoint(), "https://hf-mirror.com");
    }

    /// M-17: endpoint 访问器测试
    #[test]
    fn test_endpoint_accessor() {
        let api = HubApi::with_endpoint("https://custom-hub.example.com".to_string()).unwrap();
        assert_eq!(api.endpoint(), "https://custom-hub.example.com");
    }

    /// M-17: 无 token 时 is_authenticated 返回 false
    #[test]
    fn test_is_authenticated_without_token() {
        // 清除环境变量以避免干扰
        let _guard = test_env_guard();
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        // 无 HF_TOKEN 时应为 false
        assert!(!api.is_authenticated());
    }

    /// M-17: download_url 正确拼接 LFS resolve URL
    #[test]
    fn test_download_url() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.download_url("bert-base-uncased", "main", "config.json");
        assert_eq!(
            url,
            "https://huggingface.co/bert-base-uncased/resolve/main/config.json"
        );
    }

    /// M-17: download_url 使用自定义 endpoint
    #[test]
    fn test_download_url_custom_endpoint() {
        let api = HubApi::with_endpoint("https://hf-mirror.com".to_string()).unwrap();
        let url = api.download_url("gpt2", "v1.0", "model.safetensors");
        assert_eq!(
            url,
            "https://hf-mirror.com/gpt2/resolve/v1.0/model.safetensors"
        );
    }

    /// M-17: download_url 带子路径的文件
    #[test]
    fn test_download_url_nested_path() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.download_url("org/model", "main", "subdir/file.bin");
        assert_eq!(
            url,
            "https://huggingface.co/org/model/resolve/main/subdir/file.bin"
        );
    }

    /// 环境变量隔离守卫
    ///
    /// 测试期间移除 HF_TOKEN,测试结束后恢复原值。
    /// 使用 RAII 模式确保恢复。
    fn test_env_guard() -> EnvGuard {
        let original = std::env::var("HF_TOKEN").ok();
        // Safety: 测试代码中临时修改环境变量,仅用于隔离测试环境
        unsafe {
            std::env::remove_var("HF_TOKEN");
        }
        EnvGuard { original }
    }

    struct EnvGuard {
        original: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Safety: 测试代码中恢复环境变量,仅用于隔离测试环境
            unsafe {
                if let Some(ref val) = self.original {
                    std::env::set_var("HF_TOKEN", val);
                } else {
                    std::env::remove_var("HF_TOKEN");
                }
            }
        }
    }
}
