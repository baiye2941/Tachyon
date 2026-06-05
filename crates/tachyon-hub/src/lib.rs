//! tachyon-hub — HuggingFace Hub API 客户端
//!
//! 提供 HF Hub 的模型仓库文件浏览和下载 URL 解析功能。
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use tachyon_hub::HubClient;
//!
//! # async fn example() {
//! let hub = HubClient::from_env();
//! let files = hub.list_files("bert-base-uncased", "main").await.unwrap();
//! for f in &files {
//!     let url = hub.download_url("bert-base-uncased", "main", &f.path);
//!     println!("{} -> {url}", f.path);
//! }
//! # }
//! ```

pub mod api;
pub mod lfs;
pub mod token;

pub use api::{HfFile, HfLfsInfo, HubApi};

/// 便捷入口: 从环境变量创建 Hub 客户端
pub type HubClient = HubApi;
