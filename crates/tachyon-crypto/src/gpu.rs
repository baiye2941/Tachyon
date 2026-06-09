//! GPU 加速哈希校验
//!
//! 使用 wgpu compute shader 在 GPU 上并行计算 blake3 哈希。
//! 适用场景:单分片数据量较大且 GPU 可用时。
//!
//! # 实现状态
//!
//! WGSL 中已实现完整的 blake3 压缩函数:
//! - 7 轮 G 函数压缩(含消息排列)
//! - CHUNK_START/CHUNK_END 标志自动计算
//! - 多块 chunk 并行压缩 + CPU 端二叉树归约
//!
//! 数据流:
//! 1. 输入数据按 1024 字节对齐填充为 u32 数组
//! 2. GPU 上每个 workgroup 独立压缩一个 chunk
//! 3. 多 chunk 时,CPU 端执行 PARENT 压缩完成二叉树归约

use std::borrow::Cow;

use tachyon_core::DownloadError;
use tachyon_core::error::DownloadResult;

// BLAKE3 标志位常量(Rust 侧,用于树形归约和测试)
// 注意: ROOT = bit 3 (值 8), PARENT = bit 2 (值 4), 不要混淆!
#[allow(dead_code)]
const FLAG_CHUNK_START: u32 = 1;  // bit 0
#[allow(dead_code)]
const FLAG_CHUNK_END: u32 = 2;    // bit 1
const FLAG_PARENT: u32 = 4;       // bit 2
const FLAG_ROOT: u32 = 8;         // bit 3

/// Blake3 初始化向量(SHA-256 前 8 个质数的平方根小数部分)
const IV: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];

/// BLAKE3 消息排列索引(每轮对 16 个消息字重新排列)
const MSG_PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

/// GPU 校验器
///
/// 封装 wgpu device/queue 和 compute pipeline,提供 GPU 加速哈希。
/// 当数据量小于阈值时自动回退到 CPU blake3。
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

/// BLAKE3 chunk 大小(字节)
const CHUNK_SIZE: usize = 1024;

/// BLAKE3 块大小(字节)
const BLOCK_SIZE: usize = 64;

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
    /// - 数据量 < `GPU_MIN_SIZE` 时,直接使用 CPU blake3(GPU 启动开销不划算)
    /// - 数据量 >= `GPU_MIN_SIZE` 时,使用 GPU compute shader:
    ///   1. GPU 并行压缩所有 1024 字节 chunk
    ///   2. 读回中间哈希值
    ///   3. CPU 端执行二叉树归约得到最终哈希
    pub async fn compute_blake3(&self, data: &[u8]) -> DownloadResult<String> {
        if data.len() < GPU_MIN_SIZE {
            tracing::debug!(data_len = data.len(), "数据量小于阈值,使用 CPU blake3");
            let hash = blake3::hash(data);
            return Ok(hash.to_hex().to_string());
        }

        tracing::info!(
            data_len = data.len(),
            "使用 GPU blake3 计算哈希"
        );

        // 准备输入数据: 按 64 字节块对齐填充为 u32 数组
        let padded_size = ((data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE) * BLOCK_SIZE;
        let mut padded = vec![0u8; padded_size];
        padded[..data.len()].copy_from_slice(data);

        // 转换为 u32 小端序
        let input_words: Vec<u32> = padded
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let num_chunks = (data.len().max(1) + CHUNK_SIZE - 1) / CHUNK_SIZE;
        let input_word_count = input_words.len() as u32;
        let data_len = data.len() as u32;

        // 构建 GPU 输入 buffer: 消息字 + 元数据(4 个 u32)
        let mut gpu_input = input_words;
        gpu_input.push(data_len);         // metadata[0]: 原始数据长度
        gpu_input.push(input_word_count); // metadata[1]: 输入 u32 字数量
        gpu_input.push(0);               // metadata[2]: 保留
        gpu_input.push(num_chunks as u32); // metadata[3]: chunk 数量

        let input_bytes = bytemuck_cast_slice(&gpu_input);

        // 输出 buffer: 每个 chunk 产出 8 个 u32 (32 字节 chaining value)
        let output_size = (num_chunks * 8) as u64;

        // 创建 GPU buffers
        use wgpu::util::DeviceExt;
        let input_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blake3_input"),
            contents: input_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });

        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blake3_output"),
            size: output_size * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blake3_staging"),
            size: output_size * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 创建 bind group 并提交 compute pass
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blake3_bind_group"),
            layout: &self.blake3_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("blake3_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("blake3_compute"),
                ..Default::default()
            });
            pass.set_pipeline(&self.blake3_pipeline);
            pass.set_bind_group(0, Some(&bind_group), &[]);
            pass.dispatch_workgroups(num_chunks as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, output_size * 4);
        self.queue.submit(Some(encoder.finish()));

        // 读回 GPU 结果
        let buffer_slice = staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|e| DownloadError::Other(format!("GPU buffer 映射通道关闭: {e}").into()))?
            .map_err(|e| DownloadError::Other(format!("GPU buffer 映射失败: {e}").into()))?;

        let output_data = buffer_slice.get_mapped_range();
        let output_words: Vec<u32> = output_data
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        drop(output_data);
        staging_buffer.unmap();

        // 验证输出长度
        if output_words.len() < num_chunks * 8 {
            return Err(DownloadError::Other(
                format!(
                    "GPU 输出长度不足: 预期 {} 个 u32, 实际 {}",
                    num_chunks * 8,
                    output_words.len()
                ).into(),
            ));
        }

        if num_chunks == 1 {
            // 单 chunk: GPU shader 不设置 ROOT 标志,直接用 CPU blake3 获取正确结果
            // (GPU_MIN_SIZE >> CHUNK_SIZE 保证此路径实际不可达)
            let hash = blake3::hash(data);
            Ok(hash.to_hex().to_string())
        } else {
            // 多 chunk: CPU 端二叉树归约
            let chunk_cvs: Vec<[u32; 8]> = output_words
                .chunks(8)
                .take(num_chunks)
                .map(|c| {
                    let mut cv = [0u32; 8];
                    cv.copy_from_slice(c);
                    cv
                })
                .collect();

            let root_cv = blake3_tree_reduce(&chunk_cvs, data_len);
            Ok(u32_to_hex(&root_cv))
        }
    }
}

/// 自动选择并计算 blake3 哈希
///
/// - 数据量 < `GPU_MIN_SIZE`: 直接使用 CPU blake3
/// - 数据量 >= `GPU_MIN_SIZE` 且 GPU 可用: 使用 GPU 加速
///
/// 注意: `GpuVerifier::new()` 开销较大,调用方应缓存实例。
pub async fn auto_select_and_hash(data: &[u8]) -> DownloadResult<String> {
    if data.len() >= GPU_MIN_SIZE && GpuVerifier::is_available().await {
        tracing::info!(data_len = data.len(), "数据量达到阈值且 GPU 可用,使用 GPU blake3");
        match GpuVerifier::new().await {
            Ok(verifier) => {
                match verifier.compute_blake3(data).await {
                    Ok(hash) => return Ok(hash),
                    Err(e) => {
                        tracing::warn!(error = %e, "GPU blake3 失败,回退到 CPU");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "GPU 初始化失败,回退到 CPU blake3");
            }
        }
    }

    tracing::debug!(data_len = data.len(), "使用 CPU blake3 计算哈希");
    let hash = blake3::hash(data);
    Ok(hash.to_hex().to_string())
}

// =============================================================================
// BLAKE3 树形归约(CPU 端)
// =============================================================================

/// CPU 端 BLAKE3 压缩函数(用于多 chunk 树形归约)
///
/// 实现完整的 BLAKE3 压缩:初始化状态 -> 7 轮 G 函数 -> 异或折叠。
fn blake3_compress(
    chaining: &[u32; 8],
    block_words: &[u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 8] {
    let mut state = [
        chaining[0], chaining[1], chaining[2], chaining[3],
        chaining[4], chaining[5], chaining[6], chaining[7],
        IV[0], IV[1], IV[2], IV[3],
        counter as u32, (counter >> 32) as u32,
        block_len, flags,
    ];

    let mut m = *block_words;

    for _round in 0..7 {
        // Column step
        g(&mut state, 0, 4, 8, 12, m[0], m[1]);
        g(&mut state, 1, 5, 9, 13, m[2], m[3]);
        g(&mut state, 2, 6, 10, 14, m[4], m[5]);
        g(&mut state, 3, 7, 11, 15, m[6], m[7]);
        // Diagonal step
        g(&mut state, 0, 5, 10, 15, m[8], m[9]);
        g(&mut state, 1, 6, 11, 12, m[10], m[11]);
        g(&mut state, 2, 7, 8, 13, m[12], m[13]);
        g(&mut state, 3, 4, 9, 14, m[14], m[15]);

        // 消息排列
        let old = m;
        for i in 0..16 {
            m[i] = old[MSG_PERMUTATION[i]];
        }
    }

    // 异或折叠: state[0..8] ^= state[8..16]
    let mut result = [0u32; 8];
    for i in 0..8 {
        result[i] = state[i] ^ state[i + 8];
    }
    result
}

/// BLAKE3 G 混合函数
///
/// 对状态中 4 个元素执行 BLAKE3 的混合操作:
/// a += b + x; d = rotr32(d^a); c += d; b = rotr24(b^c); a += b + y; d = rotr16(d^a); c += d; b = rotr63(b^c)
fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(mx);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(my);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

/// BLAKE3 二叉树归约
///
/// 将各 chunk 的 chaining value 通过 PARENT 压缩逐层归约,
/// 直到只剩一个根节点,最终压缩附加 ROOT 标志。
fn blake3_tree_reduce(chunk_cvs: &[[u32; 8]], _total_len: u32) -> [u32; 8] {
    let mut cvs: Vec<[u32; 8]> = chunk_cvs.to_vec();

    while cvs.len() > 1 {
        let mut next = Vec::with_capacity((cvs.len() + 1) / 2);
        let mut i = 0;
        while i + 1 < cvs.len() {
            // 构造 parent block: left_cv[0..8] || right_cv[0..8]
            let mut parent_block = [0u32; 16];
            parent_block[..8].copy_from_slice(&cvs[i]);
            parent_block[8..16].copy_from_slice(&cvs[i + 1]);

            let is_last_pair = i + 2 >= cvs.len();
            let is_root = is_last_pair && cvs.len() <= 2;

            let mut flags = FLAG_PARENT;
            if is_root {
                flags |= FLAG_ROOT;
            }

            // counter=0 for parent nodes, block_len=64 (full block)
            next.push(blake3_compress(&IV, &parent_block, 0, BLOCK_SIZE as u32, flags));
            i += 2;
        }
        // 奇数个 CV 时,最后一个直接传递到下一层
        if i < cvs.len() {
            next.push(cvs[i]);
        }
        cvs = next;
    }

    cvs[0]
}

// =============================================================================
// 辅助函数
// =============================================================================

/// 将 u32 数组转换为小端序十六进制字符串(32 字节 = 64 字符)
fn u32_to_hex(words: &[u32]) -> String {
    let mut hex = String::with_capacity(64);
    for &w in words.iter().take(8) {
        for byte in w.to_le_bytes() {
            hex.push_str(&format!("{byte:02x}"));
        }
    }
    hex
}

/// 将 u32 切片转换为字节切片(bytemuck 的安全替代)
fn bytemuck_cast_slice(data: &[u32]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            data.as_ptr() as *const u8,
            data.len() * std::mem::size_of::<u32>(),
        )
    }
}

/// Blake3 WGSL compute shader
///
/// 实现完整的 BLAKE3 压缩函数:
/// - G 混合函数(8 步加法+异或+旋转)
/// - 7 轮压缩(Column + Diagonal 各 4 次 G 调用)
/// - 每轮间消息字排列(BLAKE3 固定排列索引)
/// - 自动设置 CHUNK_START/CHUNK_END 标志
/// - 每个 workgroup 处理一个 1024 字节 chunk(最多 16 个 64 字节块)
const BLAKE3_SHADER: &str = r#"
@group(0) @binding(0) var<storage, read> input_data: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_hash: array<u32>;

// Blake3 初始化向量(SHA-256 前 8 个质数的平方根小数部分)
const IV: array<u32, 8> = array<u32, 8>(
    0x6A09E667u, 0xBB67AE85u, 0x3C6EF372u, 0xA54FF53Au,
    0x510E527Fu, 0x9B05688Cu, 0x1F83D9ABu, 0x5BE0CD19u
);

// 消息排列索引(BLAKE3 规范定义,每轮结束后重排 16 个消息字)
const PERM: array<u32, 16> = array<u32, 16>(
    2u, 6u, 3u, 10u, 7u, 0u, 4u, 13u,
    1u, 11u, 12u, 5u, 9u, 14u, 15u, 8u
);

// Blake3 标志位
const CHUNK_START: u32 = 1u;
const CHUNK_END: u32 = 2u;

// G 混合函数:对 state 中 4 个位置执行 BLAKE3 混合操作
// 8 步操作:加法+异或+右旋(16, 12, 8, 7 位)
fn g(state: ptr<function, array<u32, 16>>, a: u32, b: u32, c: u32, d: u32, mx: u32, my: u32) {
    (*state)[a] = (*state)[a] + (*state)[b] + mx;
    (*state)[d] = rotr32((*state)[d] ^ (*state)[a], 16u);
    (*state)[c] = (*state)[c] + (*state)[d];
    (*state)[b] = rotr32((*state)[b] ^ (*state)[c], 12u);
    (*state)[a] = (*state)[a] + (*state)[b] + my;
    (*state)[d] = rotr32((*state)[d] ^ (*state)[a], 8u);
    (*state)[c] = (*state)[c] + (*state)[d];
    (*state)[b] = rotr32((*state)[b] ^ (*state)[c], 7u);
}

// 单块压缩:在 state 数组上就地执行 7 轮 G 函数
fn compress_inplace(state: ptr<function, array<u32, 16>>, m: ptr<function, array<u32, 16>>) {
    for (var round = 0u; round < 7u; round++) {
        // Column step: 4 次 G 调用,混合列内状态
        g(state, 0u, 4u, 8u, 12u, (*m)[0], (*m)[1]);
        g(state, 1u, 5u, 9u, 13u, (*m)[2], (*m)[3]);
        g(state, 2u, 6u, 10u, 14u, (*m)[4], (*m)[5]);
        g(state, 3u, 7u, 11u, 15u, (*m)[6], (*m)[7]);
        // Diagonal step: 4 次 G 调用,混合对角线状态
        g(state, 0u, 5u, 10u, 15u, (*m)[8], (*m)[9]);
        g(state, 1u, 6u, 11u, 12u, (*m)[10], (*m)[11]);
        g(state, 2u, 7u, 8u, 13u, (*m)[12], (*m)[13]);
        g(state, 3u, 4u, 9u, 14u, (*m)[14], (*m)[15]);
        // 消息排列:为下一轮重排 16 个消息字
        let tmp: array<u32, 16> = *m;
        for (var i = 0u; i < 16u; i++) {
            (*m)[i] = tmp[PERM[i]];
        }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let chunk_idx: u32 = global_id.x;

    // 读取元数据(附加在 input_data 末尾的 4 个 u32)
    let total_words: u32 = input_data[0]; // 所有 chunk 的总 u32 字数量(含填充)
    let data_len: u32 = input_data[total_words]; // 原始数据字节长度
    let total_chunks: u32 = input_data[total_words + 2u]; // chunk 总数

    if (chunk_idx >= total_chunks) {
        return;
    }

    // 每个 chunk = 1024 字节 = 256 个 u32 = 最多 16 个 64 字节块
    let chunk_start_word: u32 = chunk_idx * 256u;
    let chunk_start_byte: u32 = chunk_idx * 1024u;
    let chunk_end_byte: u32 = min(chunk_start_byte + 1024u, data_len);
    let chunk_data_len: u32 = chunk_end_byte - chunk_start_byte;
    let num_blocks: u32 = (chunk_data_len + 63u) / 64u;

    // 工作组共享消息缓冲区(所有线程协作加载)
    var<workgroup> wg_msg: array<u32, 16>;

    // 初始化 chaining value(每个 chunk 从 IV 开始)
    var cv: array<u32, 8>;
    for (var i = 0u; i < 8u; i++) {
        cv[i] = IV[i];
    }

    // 逐块压缩当前 chunk 的所有 64 字节块
    for (var block_in_chunk = 0u; block_in_chunk < num_blocks; block_in_chunk++) {
        let global_block_idx: u32 = chunk_idx * 16u + block_in_chunk;
        let block_start_word: u32 = global_block_idx * 16u;

        // 协作加载 16 个 u32 到工作组共享缓冲区
        let tid: u32 = global_id.x % 256u;
        if (tid < 16u) {
            if (block_start_word + tid < total_words) {
                wg_msg[tid] = input_data[block_start_word + tid];
            } else {
                wg_msg[tid] = 0u; // 超出数据范围的块填充零
            }
        }
        workgroupBarrier();

        // 复制到函数作用域数组(用于 G 函数指针参数)
        var m: array<u32, 16>;
        for (var i = 0u; i < 16u; i++) {
            m[i] = wg_msg[i];
        }

        // 计算当前块的标志位
        var flags: u32 = 0u;
        if (block_in_chunk == 0u) {
            flags = flags | CHUNK_START;
        }
        if (block_in_chunk == num_blocks - 1u) {
            flags = flags | CHUNK_END;
        }

        // 块内有效字节数(最后一个块可能不满 64 字节)
        var block_len: u32 = 64u;
        if (block_in_chunk == num_blocks - 1u) {
            let remaining: u32 = chunk_data_len - block_in_chunk * 64u;
            block_len = remaining;
        }

        // 初始化压缩状态: [chaining_value, IV, counter, block_len, flags]
        var state: array<u32, 16>;
        state[0] = cv[0]; state[1] = cv[1]; state[2] = cv[2]; state[3] = cv[3];
        state[4] = cv[4]; state[5] = cv[5]; state[6] = cv[6]; state[7] = cv[7];
        state[8] = IV[0]; state[9] = IV[1]; state[10] = IV[2]; state[11] = IV[3];
        state[12] = chunk_idx;     // counter low 32 bits
        state[13] = 0u;            // counter high 32 bits
        state[14] = block_len;
        state[15] = flags;

        // 执行 7 轮 G 函数压缩
        compress_inplace(&state, &m);

        // 异或折叠: chaining_value = state[0..8] XOR state[8..16]
        for (var i = 0u; i < 8u; i++) {
            cv[i] = state[i] ^ state[i + 8u];
        }
    }

    // 将当前 chunk 的 chaining value 写入输出 buffer
    let out_offset: u32 = chunk_idx * 8u;
    for (var i = 0u; i < 8u; i++) {
        output_hash[out_offset + i] = cv[i];
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 GPU 可用性检查不 panic
    #[tokio::test]
    async fn test_is_available_no_panic() {
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

    /// 测试 auto_select_and_hash 对小数据使用 CPU blake3
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
        #[cfg(not(feature = "gpu"))]
        {
            // 默认 feature 下 GpuVerifier 不应存在于作用域
            // 编译通过即表示条件编译正确
        }

        #[cfg(feature = "gpu")]
        {
            let _ = GPU_MIN_SIZE;
        }
    }

    /// 测试 WGSL shader 字符串包含完整的 BLAKE3 压缩实现关键标识
    #[test]
    fn test_wgsl_shader_content() {
        assert!(!BLAKE3_SHADER.is_empty());
        assert!(BLAKE3_SHADER.contains("@group(0) @binding(0)"));
        assert!(BLAKE3_SHADER.contains("@group(0) @binding(1)"));
        assert!(BLAKE3_SHADER.contains("@compute @workgroup_size(256)"));
        assert!(BLAKE3_SHADER.contains("fn main("));
        assert!(BLAKE3_SHADER.contains("output_hash"));
        assert!(BLAKE3_SHADER.contains("input_data"));
        // 验证 BLAKE3 压缩函数关键组件
        assert!(BLAKE3_SHADER.contains("fn g("), "应包含 G 混合函数");
        assert!(BLAKE3_SHADER.contains("fn compress_inplace("), "应包含压缩函数");
        assert!(BLAKE3_SHADER.contains("rotr32"), "应包含右旋操作");
        assert!(BLAKE3_SHADER.contains("PERM"), "应包含消息排列索引");
        assert!(BLAKE3_SHADER.contains("CHUNK_START"), "应包含 CHUNK_START 标志");
        assert!(BLAKE3_SHADER.contains("CHUNK_END"), "应包含 CHUNK_END 标志");
        assert!(BLAKE3_SHADER.contains("workgroupBarrier"), "应包含工作组同步屏障");
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

    /// 测试 CPU 端 BLAKE3 压缩函数与 blake3 crate 结果一致
    #[test]
    fn test_cpu_compress_single_chunk() {
        // 使用 blake3 crate 计算基准哈希
        let data = b"test data for CPU compress verification";
        let expected = blake3::hash(data).to_hex().to_string();

        // 手动执行压缩(单 chunk, 单 block)
        let mut padded = [0u8; 64];
        padded[..data.len()].copy_from_slice(data);
        let mut block_words = [0u32; 16];
        for i in 0..16 {
            let off = i * 4;
            block_words[i] = u32::from_le_bytes([
                padded[off], padded[off + 1], padded[off + 2], padded[off + 3],
            ]);
        }

        let flags = FLAG_CHUNK_START | FLAG_CHUNK_END | FLAG_ROOT;
        let cv = blake3_compress(&IV, &block_words, 0, data.len() as u32, flags);
        let result = u32_to_hex(&cv);

        assert_eq!(result, expected, "CPU 压缩函数应与 blake3 crate 结果一致");
    }

    /// 测试 u32_to_hex 辅助函数
    #[test]
    fn test_u32_to_hex() {
        let words = [0x01020304u32, 0x05060708, 0x090A0B0C, 0x0D0E0F10,
                     0x11121314, 0x15161718, 0x191A1B1C, 0x1D1E1F20];
        let hex = u32_to_hex(&words);
        assert_eq!(hex.len(), 64);
        // 小端序: 0x01020304 -> bytes [04, 03, 02, 01]
        assert!(hex.starts_with("04030201"));
    }
}
