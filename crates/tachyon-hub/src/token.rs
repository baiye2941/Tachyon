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

    #[test]
    fn test_load_token_from_env() {
        unsafe { std::env::set_var("HF_TOKEN", "hf_test_token_123") };
        let result = load_token();
        unsafe { std::env::remove_var("HF_TOKEN") };
        assert_eq!(result, Some("hf_test_token_123".to_string()));
    }

    #[test]
    fn test_load_token_empty_env_returns_none() {
        unsafe { std::env::set_var("HF_TOKEN", "") };
        let result = load_token();
        unsafe { std::env::remove_var("HF_TOKEN") };
        assert_eq!(result, None);
    }

    #[test]
    fn test_hf_endpoint_default() {
        unsafe { std::env::remove_var("HF_ENDPOINT") };
        assert_eq!(hf_endpoint(), "https://huggingface.co");
    }

    #[test]
    fn test_hf_endpoint_custom() {
        unsafe { std::env::set_var("HF_ENDPOINT", "https://hf-mirror.com") };
        assert_eq!(hf_endpoint(), "https://hf-mirror.com");
        unsafe { std::env::remove_var("HF_ENDPOINT") };
    }
}
