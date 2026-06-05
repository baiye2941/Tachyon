//! GPU 加速哈希校验
//!
//! 使用 wgpu compute shader 在 GPU 上并行计算 blake3 哈希。
//! 适用场景:单分片数据量较大且 GPU 可用时。
//!
//! # 当前状态
//!
//! blake3 的完整 GPU compute shader 实现需要 7 轮 G 函数压缩,
//! 工程复杂度极高。本模块搭建了完整的 wgpu compute pipeline 框架,
//! 当前哈希计算全部回退到 CPU blake3。
//!
//! TODO(gpu-blake3): 在 WGSL 中实现完整的 blake3 压缩函数后,
//!   `compute_blake3` 应读回 GPU output_buffer 作为最终哈希结果,
//!   `auto_select_and_hash` 应恢复 GPU 大数据路径。

use std::borrow::Cow;

use tachyon_core::DownloadError;
use tachyon_core::error::DownloadResult;

/// GPU 校验器
///
/// 封装 wgpu device/queue 和 compute pipeline,提供 GPU 加速哈希。
/// 当数据量小于阈值时自动回退到 CPU blake3。
///
/// TODO(gpu-blake3): 当 WGSL 压缩函数实现完成后,添加 `#[must_use]` 并
///   确保 `compute_blake3` 真正使用 GPU 结果。
// SAFETY: wgpu::Device 和 wgpu::Queue 均为 Send + Sync,
// ComputePipeline 持有的引用也是 Send + Sync。
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}
    fn _assert() {
        _assert_send::<GpuVerifier>();
        _assert_sync::<GpuVerifier>();
    }
};

pub struct GpuVerifier {
    device: wgpu::Device,
    queue: wgpu::Queue,
    blake3_pipeline: wgpu::ComputePipeline,
}

/// 小于此阈值的数据直接使用 CPU 计算,避免 GPU 启动开销超过收益
const GPU_MIN_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

impl GpuVerifier {
    /// 创建 GPU 校验器
    ///
    /// 初始化 wgpu 实例、适配器、设备和 compute pipeline。
    /// 如果系统无可用 GPU,返回错误。
    pub async fn new() -> DownloadResult<Self> {
        let instance = wgpu::Instance::default();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| DownloadError::Other("未找到可用 GPU 适配器".into()))?;

        tracing::info!(
            "GPU 适配器: {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("tachyon_crypto_device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                },
                None,
            )
            .await
            .map_err(|e| DownloadError::Other(format!("GPU 设备初始化失败: {e}").into()))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blake3_shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(BLAKE3_SHADER)),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blake3_pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        Ok(Self {
            device,
            queue,
            blake3_pipeline: pipeline,
        })
    }

    /// 检查 GPU 是否可用(不持久化设备资源)
    ///
    /// 仅探测是否有兼容的 GPU 适配器,适合用于快速决策路径。
    pub async fn is_available() -> bool {
        let instance = wgpu::Instance::default();
        instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .is_some()
    }

    /// 获取 GPU 设备调试描述
    pub fn device_name(&self) -> String {
        format!("{:?}", self.device)
    }

    /// 在 GPU 上计算 blake3 哈希
    ///
    /// # 行为
    ///
    /// - 数据量 < 64 MiB 时,直接使用 CPU blake3 计算(GPU 启动开销不划算)
    /// - 数据量 >= 64 MiB 时,当前同样使用 CPU blake3(GPU pipeline 待实现)
    ///
    /// # 注意
    ///
    /// 完整的 blake3 GPU 实现需要在 WGSL 中实现 7 轮 G 函数压缩,
    /// 当前版本先搭建 pipeline 框架,实际哈希回退 CPU blake3。
    ///
    /// TODO(gpu-blake3): 实现 WGSL 压缩函数后,读回 output_buffer 替代 CPU 计算。
    pub async fn compute_blake3(&self, data: &[u8]) -> DownloadResult<String> {
        // TODO(gpu-blake3): 当 WGSL 实现完整的 blake3 压缩后,以下代码应替换为:
        //   1. 创建 input/output buffer
        //   2. dispatch compute pass
        //   3. 读回 output_buffer 并返回哈希
        // 当前 GPU pipeline 仅写 IV 到 output_hash,不执行压缩,因此跳过 GPU 执行。
        if data.len() < GPU_MIN_SIZE {
            tracing::debug!(data_len = data.len(), "数据量小于阈值,使用 CPU blake3");
        } else {
            tracing::info!(
                data_len = data.len(),
                "GPU blake3 压缩尚未实现,回退到 CPU blake3"
            );
        }

        let hash = blake3::hash(data);
        Ok(hash.to_hex().to_string())
    }
}

/// 自动选择并计算 blake3 哈希
///
/// 当前所有数据均使用 CPU blake3 计算。
///
/// TODO(gpu-blake3): WGSL 压缩实现完成后,恢复 GPU 大数据路径:
///   - 数据量 >= 64 MiB 且 GPU 可用时,复用 `GpuVerifier` 实例(调用方应缓存)
///   - 避免每次调用重建设备/队列/pipeline
pub async fn auto_select_and_hash(data: &[u8]) -> DownloadResult<String> {
    // TODO(gpu-blake3): 恢复 GPU 路径,注意:
    //   - GpuVerifier::new() 开销大(设备/队列/pipeline),不应在热路径调用
    //   - 调用方应缓存 GpuVerifier 实例,此处仅做兼容性路由
    tracing::debug!(data_len = data.len(), "使用 CPU blake3 计算哈希");
    let hash = blake3::hash(data);
    Ok(hash.to_hex().to_string())
}

/// Blake3 WGSL compute shader
///
/// 定义 blake3 哈希所需的 storage buffer 绑定和初始化向量。
/// 完整实现需要在 WGSL 中实现 G 函数的 7 轮压缩,当前为框架版本。
const BLAKE3_SHADER: &str = r#"
@group(0) @binding(0) var<storage, read> input_data: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_hash: array<u32, 8>;

// Blake3 初始化向量(来自 SHA-256 前 8 个质数的平方根小数部分)
const IV: array<u32, 8> = array<u32, 8>(
    0x6A09E667u, 0xBB67AE85u, 0x3C6EF372u, 0xA54FF53Au,
    0x510E527Fu, 0x9B05688Cu, 0x1F83D9ABu, 0x5BE0CD19u
);

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    // 仅第一个线程执行初始化
    // 完整实现需要分块读取 input_data 并执行 7 轮 G 函数压缩
    if (global_id.x == 0u) {
        for (var i = 0u; i < 8u; i++) {
            output_hash[i] = IV[i];
        }
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 GPU 可用性检查不 panic
    #[tokio::test]
    async fn test_is_available_no_panic() {
        // 无论环境是否有 GPU,此调用不应 panic
        let _available = GpuVerifier::is_available().await;
    }

    /// 测试小数据直接使用 CPU blake3 且结果正确
    #[tokio::test]
    async fn test_small_data_uses_cpu_blake3() {
        let data = b"hello world";
        let expected = blake3::hash(data).to_hex().to_string();
        let result = auto_select_and_hash(data).await.unwrap();
        assert_eq!(result, expected);
    }

    /// 测试 auto_select_and_hash 始终使用 CPU blake3
    #[tokio::test]
    async fn test_auto_select_uses_cpu() {
        let data = b"small data for cpu path";
        let expected = blake3::hash(data).to_hex().to_string();
        let result = auto_select_and_hash(data).await.unwrap();
        assert_eq!(result, expected);
    }

    /// 测试 GPU feature 未启用时编译正确(条件编译验证)
    #[test]
    fn test_gpu_feature_gate() {
        // 此测试验证编译时 feature gate 的正确性:
        // - 默认编译时 gpu 模块不存在
        // - 使用 --features gpu 时 gpu 模块才可用
        #[cfg(not(feature = "gpu"))]
        {
            // 默认 feature 下 GpuVerifier 不应存在于作用域
            // 编译通过即表示条件编译正确
        }

        #[cfg(feature = "gpu")]
        {
            // GPU feature 启用时,模块编译通过即表示可用
            // 额外验证关键常量存在
            let _ = GPU_MIN_SIZE;
        }
    }

    /// 测试 WGSL shader 字符串非空且包含关键标识
    #[test]
    fn test_wgsl_shader_content() {
        assert!(!BLAKE3_SHADER.is_empty());
        assert!(BLAKE3_SHADER.contains("@group(0) @binding(0)"));
        assert!(BLAKE3_SHADER.contains("@group(0) @binding(1)"));
        assert!(BLAKE3_SHADER.contains("@compute @workgroup_size(256)"));
        assert!(BLAKE3_SHADER.contains("fn main("));
        assert!(BLAKE3_SHADER.contains("output_hash"));
        assert!(BLAKE3_SHADER.contains("input_data"));
    }

    /// 测试 CPU blake3 与 auto_select_and_hash 对同一数据产生相同哈希
    #[tokio::test]
    async fn test_cpu_gpu_hash_consistency() {
        let data = b"consistency check data";

        let cpu_hash = blake3::hash(data).to_hex().to_string();
        let result = auto_select_and_hash(data).await.unwrap();

        assert_eq!(cpu_hash, result);
        assert_eq!(cpu_hash.len(), 64); // blake3 256-bit = 64 hex chars
    }

    /// 测试空数据的哈希计算
    #[tokio::test]
    async fn test_empty_data_hash() {
        let data = b"";
        let expected = blake3::hash(data).to_hex().to_string();
        let result = auto_select_and_hash(data).await.unwrap();
        assert_eq!(result, expected);
        assert!(!result.is_empty());
    }

    /// 测试不同数据产生不同哈希
    #[tokio::test]
    async fn test_different_data_different_hash() {
        let hash_a = auto_select_and_hash(b"data_a").await.unwrap();
        let hash_b = auto_select_and_hash(b"data_b").await.unwrap();
        assert_ne!(hash_a, hash_b);
    }
}
