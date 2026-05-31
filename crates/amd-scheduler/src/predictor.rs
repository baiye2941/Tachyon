//! 带宽预测模型
//!
//! 基于 Holt-Winters 指数平滑的短期带宽预测。
//! 提供置信度评估,帮助调度器判断预测可靠性。

/// Holt-Winters 带宽预测器
pub struct HoltWintersPredictor {
    /// 水平分量
    level: f64,
    /// 趋势分量
    trend: f64,
    /// 水平平滑因子
    alpha: f64,
    /// 趋势平滑因子
    beta: f64,
    /// 是否已初始化
    initialized: bool,
    /// 累计观测样本数
    sample_count: u64,
}

impl HoltWintersPredictor {
    pub fn new(alpha: f64, beta: f64) -> Self {
        Self {
            level: 0.0,
            trend: 0.0,
            alpha: alpha.clamp(0.0, 1.0),
            beta: beta.clamp(0.0, 1.0),
            initialized: false,
            sample_count: 0,
        }
    }

    /// 记录新的带宽观测值(字节/秒)
    pub fn observe(&mut self, value: f64) {
        // 跳过无效值，防止 NaN/Inf 传播污染 EMA
        if !value.is_finite() || value < 0.0 {
            tracing::warn!("无效的速度值: {}, 跳过 EMA 更新", value);
            return;
        }
        self.sample_count += 1;
        if !self.initialized {
            self.level = value;
            self.trend = 0.0;
            self.initialized = true;
            return;
        }
        let new_level = self.alpha * value + (1.0 - self.alpha) * (self.level + self.trend);
        self.trend = self.beta * (new_level - self.level) + (1.0 - self.beta) * self.trend;
        self.level = new_level;
    }

    /// 预测未来第 steps 步的带宽(不会返回负值)
    pub fn predict(&self, steps: u64) -> f64 {
        if !self.initialized {
            return 0.0;
        }
        (self.level + self.trend * steps as f64).max(0.0)
    }

    /// 当前水平估计
    pub fn current_level(&self) -> f64 {
        self.level
    }

    /// 累计观测样本数
    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    /// 预测置信度(样本越多越可信)
    ///
    /// 基于样本数量的 sigmoid 置信度函数:
    /// - 0 个样本: 0.0
    /// - 10 个样本: 约 0.5
    /// - 30+ 个样本: 接近 1.0
    ///
    /// 返回值范围 [0.0, 1.0],可用于调度器加权预测结果。
    pub fn confidence(&self) -> f64 {
        if !self.initialized {
            return 0.0;
        }
        // 基于样本数量的 sigmoid 置信度
        let n = self.sample_count as f64;
        1.0 / (1.0 + (-0.1 * (n - 10.0)).exp())
    }
}

impl Default for HoltWintersPredictor {
    fn default() -> Self {
        Self::new(0.3, 0.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predictor_initialization() {
        let pred = HoltWintersPredictor::default();
        assert_eq!(pred.predict(1), 0.0);
    }

    #[test]
    fn test_predictor_single_observation() {
        let mut pred = HoltWintersPredictor::default();
        pred.observe(100.0);
        assert_eq!(pred.current_level(), 100.0);
        assert_eq!(pred.predict(1), 100.0); // trend = 0
    }

    #[test]
    fn test_predictor_ignores_nan() {
        let mut pred = HoltWintersPredictor::default();
        pred.observe(100.0);
        pred.observe(f64::NAN);
        pred.observe(f64::INFINITY);
        pred.observe(f64::NEG_INFINITY);
        // NaN/Inf 应被跳过，level 仍为首次观测值
        assert_eq!(pred.current_level(), 100.0);
        assert_eq!(pred.sample_count(), 1);
    }

    #[test]
    fn test_predictor_ignores_negative() {
        let mut pred = HoltWintersPredictor::default();
        pred.observe(100.0);
        pred.observe(-50.0);
        // 负值应被跳过
        assert_eq!(pred.current_level(), 100.0);
        assert_eq!(pred.sample_count(), 1);
    }

    #[test]
    fn test_predictor_trend() {
        let mut pred = HoltWintersPredictor::new(0.5, 0.5);
        pred.observe(100.0);
        pred.observe(200.0);
        // 水平和趋势应该上升
        assert!(pred.predict(1) > 100.0);
    }

    #[test]
    fn test_predictor_stable() {
        let mut pred = HoltWintersPredictor::default();
        for _ in 0..10 {
            pred.observe(1000.0);
        }
        let p = pred.predict(1);
        assert!((p - 1000.0).abs() < 10.0);
    }

    #[test]
    fn test_predictor_no_observations() {
        let pred = HoltWintersPredictor::default();
        assert_eq!(pred.predict(0), 0.0);
    }

    #[test]
    fn test_confidence_no_observations() {
        let pred = HoltWintersPredictor::default();
        assert_eq!(pred.confidence(), 0.0);
        assert_eq!(pred.sample_count(), 0);
    }

    #[test]
    fn test_confidence_after_one_observation() {
        let mut pred = HoltWintersPredictor::default();
        pred.observe(100.0);
        assert_eq!(pred.sample_count(), 1);
        // n=1: 1/(1+exp(-0.1*(1-10))) = 1/(1+exp(0.9)) ≈ 0.289
        let conf = pred.confidence();
        assert!(conf > 0.2 && conf < 0.4);
    }

    #[test]
    fn test_confidence_at_ten_observations() {
        let mut pred = HoltWintersPredictor::default();
        for i in 0..10 {
            pred.observe(100.0 + i as f64);
        }
        assert_eq!(pred.sample_count(), 10);
        // n=10: 1/(1+exp(0)) = 0.5
        let conf = pred.confidence();
        assert!((conf - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_confidence_high_sample_count() {
        let mut pred = HoltWintersPredictor::default();
        for _ in 0..50 {
            pred.observe(1000.0);
        }
        assert_eq!(pred.sample_count(), 50);
        // n=50: 1/(1+exp(-4)) ≈ 0.982
        let conf = pred.confidence();
        assert!(conf > 0.95);
    }

    #[test]
    fn test_sample_count_increments() {
        let mut pred = HoltWintersPredictor::default();
        assert_eq!(pred.sample_count(), 0);
        pred.observe(100.0);
        assert_eq!(pred.sample_count(), 1);
        pred.observe(200.0);
        assert_eq!(pred.sample_count(), 2);
        pred.observe(300.0);
        assert_eq!(pred.sample_count(), 3);
    }

    #[test]
    fn test_confidence_monotonic_increase() {
        let mut pred = HoltWintersPredictor::default();
        let mut prev_conf = 0.0;
        for i in 1..=20 {
            pred.observe(i as f64 * 100.0);
            let conf = pred.confidence();
            assert!(
                conf >= prev_conf,
                "置信度应单调递增: 第 {i} 个样本时 conf={conf}, prev={prev_conf}"
            );
            prev_conf = conf;
        }
    }
}
