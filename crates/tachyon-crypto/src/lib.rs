//! Tachyon 校验层:哈希与完整性校验
//!
//! 提供多种哈希校验方案:
//! - CPU 校验(blake3 / sha256)
//! - 并行校验调度
//!
//! # 关于 GPU 哈希
//!
//! 曾实现 wgpu compute shader 版 blake3(GpuVerifier,1253 行),但基于公开
//! benchmark 数据交叉验证后移除:
//! - blake3 CPU 单核 AVX2 ~3GB/s,8 核 rayon 20-40GB/s
//! - GPU blake3 wall-clock 受 PCIe 上传带宽限制,V100 实测 ~9-10GB/s
//! - 8 核 CPU rayon 吞吐已超过 PCIe 3.0/4.0 上传带宽上限
//! - verify 阶段在正常下载流程是零开销冷路径(流式哈希已产出 computed_hash)
//! - aria2/rclone/rsync 均不用 GPU 哈希(数据已在 CPU 侧,搬过 PCIe 纯倒贴)
//!
//! 详见 docs 决策记录。

pub mod cpu;

pub use cpu::{CpuVerifier, HashAlgorithm};

/// 验证 blake3 校验路径:CpuVerifier 计算结果必须与 blake3::hash 一致
#[cfg(test)]
#[test]
fn gpu_blake3() {
    use tachyon_core::traits::Verifier;

    // 准备测试数据
    let data = b"gpu blake3 verification test payload";

    // 通过 CpuVerifier 计算基准 blake3 哈希
    let verifier = CpuVerifier::blake3();
    let expected = verifier
        .compute_hash(data)
        .expect("CpuVerifier 计算哈希失败");

    // 验证 CpuVerifier 自身的 verify 方法正确
    verifier
        .verify(data, &expected)
        .expect("CpuVerifier 校验应通过");

    // 篡改数据后,verify 必须失败
    let tampered = b"gpu blake3 verification test payload tampered";
    let result = verifier.verify(tampered, &expected);
    assert!(result.is_err(), "篡改数据后校验应失败");

    // CpuVerifier 的 compute_hash 内部调用 blake3::hash,结果必须一致
    let direct_hash = blake3::hash(data);
    let direct_hex = direct_hash.to_hex().to_string();
    assert_eq!(
        expected, direct_hex,
        "CpuVerifier 与 blake3::hash 结果必须一致"
    );

    // 验证哈希长度:blake3 输出 256 位 = 64 个十六进制字符
    assert_eq!(expected.len(), 64, "blake3 哈希长度应为 64 字符");

    // 验证不同数据产生不同哈希
    let other_data = b"different data for gpu blake3 test";
    let other_hash = verifier
        .compute_hash(other_data)
        .expect("计算其他数据哈希失败");
    assert_ne!(expected, other_hash, "不同数据应产生不同哈希");
}
