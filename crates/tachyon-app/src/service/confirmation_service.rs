//! 一次性确认令牌服务
//!
//! 为破坏性命令(delete_task/update_config)提供二次确认机制。
//! 前端在用户确认后先请求 token,再将 token 传入破坏性命令完成操作。
//! Token 60 秒后自动失效,使用后立即销毁(一次性)。
//!
//! 安全属性:
//! - 一次性: validate_and_consume 原子移除 token,重放攻击无效
//! - 时效性: 60 秒过期,限制攻击窗口
//! - 不可预测: UUID v4 随机生成,暴力枚举不可行
//! - 动作绑定: token 绑定到特定 action,无法跨操作复用
//! - 容量上限: 最多 1024 个并发 token,超出时强制清理过期项

use std::sync::atomic::{AtomicBool, Ordering};

use crate::commands::AppError;
use crate::commands::ConfirmationStore;

/// 确认令牌服务
///
/// 对 `ConfirmationStore` 的薄封装,将 token 生成/校验逻辑从 AppState 和 command
/// 中抽离出来,避免 AppState 成为 God Object 后各命令直接操作底层存储字段。
pub struct ConfirmationService {
    store: ConfirmationStore,
}

impl ConfirmationService {
    /// 创建新的确认令牌服务
    pub fn new() -> Self {
        Self {
            store: ConfirmationStore::new(),
        }
    }

    /// 请求一个绑定到指定 action 的一次性确认令牌
    ///
    /// 当 token 数量超过容量上限时,先强制清理过期项再插入。
    /// 若清理后仍超限,返回明确错误而非空字符串(S-04)。
    pub fn request(&self, action: &str) -> Result<String, AppError> {
        tracing::info!(action = %action, "请求确认令牌");
        let token = self.store.generate(action);
        // 清理过期 token(低频操作,每次请求时顺便清理)
        self.store.cleanup_expired();
        if token.is_empty() {
            Err(AppError::Config("确认令牌服务繁忙,请稍后重试".to_string()))
        } else {
            Ok(token)
        }
    }

    /// 验证并消费 token(一次性)
    ///
    /// token 必须存在、未过期且 action 匹配,否则返回 Config 错误。
    pub fn validate_and_consume(&self, token: &str, action: &str) -> Result<(), AppError> {
        if self.store.validate_and_consume(token, action) {
            Ok(())
        } else {
            Err(AppError::Config(
                "确认令牌无效、已过期或与操作不匹配,请重新确认".to_string(),
            ))
        }
    }
}

impl Default for ConfirmationService {
    fn default() -> Self {
        Self::new()
    }
}

/// 原子地声明进度订阅权
///
/// 首次调用将 `flag` 从 `false` 原子切换到 `true` 并返回 `true`(调用方获得订阅权);
/// 后续调用 `flag` 已是 `true`,直接返回 `false`(调用方应跳过 spawn)。
///
/// 使用 `compare_exchange` 保证 check-and-set 的原子性,防止并发 subscribe_progress
/// 竞态下 spawn 多个后台 broker 任务。
pub fn try_claim_subscription(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_and_validate_token() {
        let service = ConfirmationService::new();
        let token = service.request("delete_task").unwrap();
        assert!(!token.is_empty());
        assert!(service.validate_and_consume(&token, "delete_task").is_ok());
        assert!(service.validate_and_consume(&token, "delete_task").is_err());
    }

    #[test]
    fn test_validate_wrong_action_fails() {
        let service = ConfirmationService::new();
        let token = service.request("delete_task").unwrap();
        assert!(
            service
                .validate_and_consume(&token, "update_config")
                .is_err()
        );
    }

    #[test]
    fn test_try_claim_subscription_first_call_returns_true() {
        let flag = AtomicBool::new(false);
        assert!(try_claim_subscription(&flag));
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn test_try_claim_subscription_second_call_returns_false() {
        let flag = AtomicBool::new(true);
        assert!(!try_claim_subscription(&flag));
    }
}
