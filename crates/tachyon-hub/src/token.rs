//! HF Token 管理
//!
//! 加载顺序: HF_TOKEN 环境变量 > ~/.huggingface/token 文件

/// 从环境变量或文件加载 HuggingFace Token
///
/// 优先级:
/// 1. `HF_TOKEN` 环境变量
/// 2. `~/.huggingface/token` 文件 (huggingface_hub 标准路径)
///
/// 返回 `None` 表示未配置 Token (匿名访问)。
pub fn load_token() -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Ok(t) = std::env::var("HUGGINGFACE_HUB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }

    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)?;
    let token_path = home.join(".huggingface").join("token");
    std::fs::read_to_string(&token_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// HF_ENDPOINT 镜像地址 (默认为 https://huggingface.co)
pub fn hf_endpoint() -> String {
    std::env::var("HF_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://huggingface.co".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvSnapshot {
        key: &'static str,
        value: Option<OsString>,
    }

    impl EnvSnapshot {
        fn capture(key: &'static str) -> Self {
            Self {
                key,
                value: std::env::var_os(key),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            match &self.value {
                // Safety: 测试代码中恢复环境变量,仅用于隔离测试环境。
                // ENV_LOCK 保证同一时刻仅一个测试线程操作环境变量,
                // 且 Drop 在 EnvSnapshot 离开作用域时调用,与捕获配对。
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                // Safety: 同上,移除测试中捕获时已不存在的环境变量。
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn isolate_token_fallbacks() -> Vec<EnvSnapshot> {
        let snapshots = vec![
            EnvSnapshot::capture("HF_TOKEN"),
            EnvSnapshot::capture("HUGGINGFACE_HUB_TOKEN"),
            EnvSnapshot::capture("HOME"),
            EnvSnapshot::capture("USERPROFILE"),
        ];
        // Safety: 测试隔离 — ENV_LOCK 保证线程安全,EnvSnapshot::drop 在作用域结束时恢复。
        unsafe { std::env::remove_var("HUGGINGFACE_HUB_TOKEN") };
        let isolated_home =
            std::env::temp_dir().join(format!("tachyon-hub-token-test-{}", std::process::id()));
        // Safety: 同上,设置 HOME 供测试用,Drop 时恢复原值。
        unsafe { std::env::set_var("HOME", &isolated_home) };
        // Safety: 同上,移除 USERPROFILE 避免跨平台干扰。
        unsafe { std::env::remove_var("USERPROFILE") };
        snapshots
    }

    #[test]
    fn test_load_token_from_env() {
        let _guard = env_lock();
        let _env = isolate_token_fallbacks();
        // Safety: 测试隔离 — ENV_LOCK 保证线程安全,函数结束时 remove_var 恢复。
        unsafe { std::env::set_var("HF_TOKEN", "hf_test_token_123") };
        let result = load_token();
        // Safety: 同上,清理测试设置的环境变量。
        unsafe { std::env::remove_var("HF_TOKEN") };
        assert_eq!(result, Some("hf_test_token_123".to_string()));
    }

    #[test]
    fn test_load_token_empty_env_returns_none() {
        let _guard = env_lock();
        let _env = isolate_token_fallbacks();
        // Safety: 测试隔离 — ENV_LOCK 保证线程安全,函数结束时 remove_var 恢复。
        unsafe { std::env::set_var("HF_TOKEN", "") };
        let result = load_token();
        // Safety: 同上,清理测试设置的环境变量。
        unsafe { std::env::remove_var("HF_TOKEN") };
        assert_eq!(result, None);
    }

    #[test]
    fn test_hf_endpoint_default() {
        let _guard = env_lock();
        let _endpoint = EnvSnapshot::capture("HF_ENDPOINT");
        // Safety: 测试隔离 — ENV_LOCK 保证线程安全,EnvSnapshot::drop 恢复原值。
        unsafe { std::env::remove_var("HF_ENDPOINT") };
        assert_eq!(hf_endpoint(), "https://huggingface.co");
    }

    #[test]
    fn test_hf_endpoint_custom() {
        let _guard = env_lock();
        let _endpoint = EnvSnapshot::capture("HF_ENDPOINT");
        // Safety: 测试隔离 — ENV_LOCK 保证线程安全,EnvSnapshot::drop 恢复原值。
        unsafe { std::env::set_var("HF_ENDPOINT", "https://hf-mirror.com") };
        assert_eq!(hf_endpoint(), "https://hf-mirror.com");
        // Safety: 同上,清理测试设置的环境变量。
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }
}
