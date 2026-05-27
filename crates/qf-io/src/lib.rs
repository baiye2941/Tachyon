//! QuantumFetch I/O 层:零拷贝存储引擎
//!
//! 提供跨平台的异步文件 I/O:
//! - Linux:io_uring 零拷贝管道
//! - Windows/macOS:tokio 标准异步文件 I/O
//! - BufferPool 管理与 buffer 复用
//! - 零拷贝写入管道

pub mod buffer;
pub mod pipeline;
pub mod storage;
pub mod tokio_file;

pub use buffer::BufferPool;
pub use pipeline::WritePipeline;
pub use storage::AsyncStorage;
pub use tokio_file::TokioFile;
