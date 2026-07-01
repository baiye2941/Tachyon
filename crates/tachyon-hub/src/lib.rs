//! tachyon-hub — HuggingFace Hub API 客户端
//!
//! 提供 HF Hub 的模型仓库文件浏览和下载 URL 解析功能。
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use tachyon_core::config::HubConfig;
//! use tachyon_hub::HubApi;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = HubConfig::default();
//! let hub = HubApi::from_config(&config)?;
//! let files = hub.list_files("bert-base-uncased", "main").await?;
//! for f in &files {
//!     let url = hub.download_url("bert-base-uncased", "main", &f.path);
//!     println!("{} -> {url}", f.path);
//! }
//! # Ok(())
//! # }
//! ```

pub mod api;
pub mod classify;
pub mod lfs;
pub mod token;

pub use api::{HfCardData, HfFile, HfLfsInfo, HfModelInfo, HubApi};
pub use classify::{FileCategory, classify_file};
