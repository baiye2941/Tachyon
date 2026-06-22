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

/// 模型卡片摘要数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfCardData {
    /// 模型描述
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 支持语言
    #[serde(default)]
    pub language: Vec<String>,
    /// 关联数据集
    #[serde(default)]
    pub datasets: Vec<String>,
}

/// HuggingFace 模型元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelInfo {
    /// 仓库 ID, 格式 "owner/repo"
    pub id: String,
    /// 作者
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// 当前 commit hash
    pub sha: String,
    /// 最后修改时间 (ISO 8601)
    #[serde(default)]
    pub last_modified: String,
    /// 标签列表
    #[serde(default)]
    pub tags: Vec<String>,
    /// Pipeline 标签, 如 "text-classification"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_tag: Option<String>,
    /// 框架名称, 如 "pytorch"/"transformers"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_name: Option<String>,
    /// 许可证
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// 下载次数
    #[serde(default)]
    pub downloads: u64,
    /// 点赞数
    #[serde(default)]
    pub likes: u64,
    /// 文件列表
    #[serde(default)]
    pub siblings: Vec<HfFile>,
    /// 模型卡片摘要
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_data: Option<HfCardData>,
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

    /// 获取模型元数据
    ///
    /// GET {endpoint}/api/models/{repo_id}
    pub async fn model_info(&self, repo_id: &str, revision: &str) -> DownloadResult<HfModelInfo> {
        let mut url = format!("{}/api/models/{repo_id}", self.endpoint);
        if revision != "main" {
            url = format!("{url}?revision={revision}");
        }
        tracing::info!(url = %url, repo_id = %repo_id, "获取 HF 模型元数据");

        let mut headers: Vec<(&str, &str)> = vec![("User-Agent", "tachyon-hub/0.1.0")];
        let auth;
        if let Some(ref token) = self.token {
            auth = format!("Bearer {token}");
            headers.push(("Authorization", &auth));
        }

        let body = self.http.get_text(&url, &headers).await?;
        let info: HfModelInfo =
            serde_json::from_str(&body).map_err(tachyon_core::DownloadError::Serialization)?;

        tracing::info!(repo_id = %repo_id, "获取模型元数据成功");
        Ok(info)
    }

    /// 搜索模型
    ///
    /// GET {endpoint}/api/models?search={query}&limit={limit}
    pub async fn search_models(&self, query: &str, limit: u32) -> DownloadResult<Vec<HfModelInfo>> {
        let encoded = urlencoding::encode(query);
        let url = format!(
            "{}/api/models?search={encoded}&limit={limit}",
            self.endpoint
        );
        tracing::info!(url = %url, query = %query, "搜索 HF 模型");

        let mut headers: Vec<(&str, &str)> = vec![("User-Agent", "tachyon-hub/0.1.0")];
        let auth;
        if let Some(ref token) = self.token {
            auth = format!("Bearer {token}");
            headers.push(("Authorization", &auth));
        }

        let body = self.http.get_text(&url, &headers).await?;
        let models: Vec<HfModelInfo> =
            serde_json::from_str(&body).map_err(tachyon_core::DownloadError::Serialization)?;

        tracing::info!(count = models.len(), query = %query, "搜索模型成功");
        Ok(models)
    }

    // 以下辅助方法仅供测试使用,用于验证 URL 构建逻辑

    /// 构建 model_info URL (测试用)
    #[cfg(test)]
    fn model_info_url(&self, repo_id: &str, revision: &str) -> String {
        let mut url = format!("{}/api/models/{repo_id}", self.endpoint);
        if revision != "main" {
            url = format!("{url}?revision={revision}");
        }
        url
    }

    /// 构建 search_models URL (测试用)
    #[cfg(test)]
    fn search_models_url(&self, query: &str, limit: u32) -> String {
        let encoded = urlencoding::encode(query);
        format!(
            "{}/api/models?search={encoded}&limit={limit}",
            self.endpoint
        )
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

    // ====== model_info / search_models URL 构建测试 ======

    /// M-17: model_info URL 构建 (main revision)
    #[test]
    fn test_model_info_url_main() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.model_info_url("bert-base-uncased", "main");
        assert_eq!(url, "https://huggingface.co/api/models/bert-base-uncased");
    }

    /// M-17: model_info URL 构建 (非 main revision)
    #[test]
    fn test_model_info_url_revision() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.model_info_url("gpt2", "v1.0");
        assert_eq!(url, "https://huggingface.co/api/models/gpt2?revision=v1.0");
    }

    /// M-17: search_models URL 构建
    #[test]
    fn test_search_models_url() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.search_models_url("bert", 10);
        assert_eq!(
            url,
            "https://huggingface.co/api/models?search=bert&limit=10"
        );
    }

    /// M-17: search_models URL 编码特殊字符
    #[test]
    fn test_search_models_url_encoding() {
        let api = HubApi::with_endpoint("https://huggingface.co".to_string()).unwrap();
        let url = api.search_models_url("bert base", 5);
        assert_eq!(
            url,
            "https://huggingface.co/api/models?search=bert%20base&limit=5"
        );
    }

    // ====== HfModelInfo / HfCardData 反序列化测试 ======

    /// M-17: HfModelInfo 完整字段反序列化
    #[test]
    fn test_model_info_deserialization_full() {
        let json = r#"{
            "id": "org/model",
            "author": "test-author",
            "sha": "abc123def456",
            "last_modified": "2024-01-15T08:30:00Z",
            "tags": ["transformers", "pytorch"],
            "pipeline_tag": "text-classification",
            "library_name": "transformers",
            "license": "apache-2.0",
            "downloads": 12345,
            "likes": 678,
            "siblings": [
                {"type": "file", "path": "config.json", "size": 1234, "lfs": null}
            ],
            "card_data": {
                "description": "A test model",
                "language": ["en"],
                "datasets": ["dataset1"]
            }
        }"#;

        let info: HfModelInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.id, "org/model");
        assert_eq!(info.author.as_deref(), Some("test-author"));
        assert_eq!(info.sha, "abc123def456");
        assert_eq!(info.last_modified, "2024-01-15T08:30:00Z");
        assert_eq!(info.tags, vec!["transformers", "pytorch"]);
        assert_eq!(info.pipeline_tag.as_deref(), Some("text-classification"));
        assert_eq!(info.library_name.as_deref(), Some("transformers"));
        assert_eq!(info.license.as_deref(), Some("apache-2.0"));
        assert_eq!(info.downloads, 12345);
        assert_eq!(info.likes, 678);
        assert_eq!(info.siblings.len(), 1);
        assert_eq!(info.siblings[0].path, "config.json");
        assert!(info.card_data.is_some());
        let card = info.card_data.unwrap();
        assert_eq!(card.description.as_deref(), Some("A test model"));
        assert_eq!(card.language, vec!["en"]);
        assert_eq!(card.datasets, vec!["dataset1"]);
    }

    /// M-17: HfModelInfo 最小字段反序列化 (缺失可选字段)
    #[test]
    fn test_model_info_deserialization_minimal() {
        let json = r#"{"id": "minimal/model", "sha": "abc123"}"#;
        let info: HfModelInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.id, "minimal/model");
        assert_eq!(info.sha, "abc123");
        assert!(info.author.is_none());
        assert!(info.last_modified.is_empty());
        assert!(info.tags.is_empty());
        assert!(info.pipeline_tag.is_none());
        assert!(info.library_name.is_none());
        assert!(info.license.is_none());
        assert_eq!(info.downloads, 0);
        assert_eq!(info.likes, 0);
        assert!(info.siblings.is_empty());
        assert!(info.card_data.is_none());
    }

    /// M-17: HfModelInfo 数组反序列化 (搜索接口返回)
    #[test]
    fn test_model_info_array_deserialization() {
        let json = r#"[
            {"id": "model/1", "sha": "sha1"},
            {"id": "model/2", "sha": "sha2", "downloads": 100}
        ]"#;
        let models: Vec<HfModelInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "model/1");
        assert_eq!(models[1].id, "model/2");
        assert_eq!(models[1].downloads, 100);
    }

    /// M-17: HfCardData 反序列化 (空对象)
    #[test]
    fn test_card_data_deserialization_empty() {
        let json = r#"{}"#;
        let card: HfCardData = serde_json::from_str(json).unwrap();
        assert!(card.description.is_none());
        assert!(card.language.is_empty());
        assert!(card.datasets.is_empty());
    }
}
