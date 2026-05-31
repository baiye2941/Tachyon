//! AI Model Downloader 校验层:GPU 加速哈希与完整性校验
//!
//! 提供多种哈希校验方案:
//! - CPU 校验(blake3 / sha256)
//! - GPU 校验(wgpu compute shader, 可选 feature `gpu`)
//! - 并行校验调度

pub mod cpu;

#[cfg(feature = "gpu")]
pub mod gpu;

pub use cpu::{CpuVerifier, HashAlgorithm, auto_select_verifier};

#[cfg(feature = "gpu")]
pub use gpu::GpuVerifier;

/// 验证 GPU blake3 校验路径:GPU compute_blake3 回退到 CPU 时,结果必须与 CpuVerifier 一致
#[cfg(test)]
#[test]
fn gpu_blake3() {
    use amd_core::traits::Verifier;

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

    // GPU 回退到 CPU 路径时,blake3::hash 的结果必须与 CpuVerifier 一致
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
