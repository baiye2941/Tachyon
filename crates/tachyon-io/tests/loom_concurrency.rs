//! Loom 并发模型验证:BufferIndexPool 与 CompletionSlots 的位图分配器。
//!
//! # 方案 B:独立复制核心逻辑
//!
//! 生产代码中 `BufferIndexPool`(`iouring.rs`,私有 + `cfg(any(test, target_os="linux"))`)
//! 与 `CompletionSlots`(`iocp.rs`,私有 + `cfg(target_os="windows")`)均非公开且受
//! 平台 cfg 门控,集成测试无法直接引用。本文件在 `#[cfg(feature = "loom")]` 下用
//! `loom::sync::atomic` 逐行复制其位图分配/释放逻辑,穷举线程交错验证并发不变量。
//!
//! 复制的逻辑与生产代码保持字节级一致(包括内存序),确保 loom 探索的状态空间
//! 与真实运行时行为同构。两套实现的差异仅在内存序:
//! - BufferIndexPool:全 `Relaxed`(位图本身是原子操作,可见性由外部 `state.store(Release)`
//!   + channel 同步间接保证)。
//! - CompletionSlots:`AcqRel` CAS(alloc/release 均带 Release 语义,配合 slot.state
//!   的 Release/Acquire 传递 pending 写入可见性)。
//!
//! # 验证的不变量
//!
//! 1. **无重复分配**:并发 alloc 永不返回相同索引(位图 CAS 保证互斥占用)。
//! 2. **不泄漏**:alloc N 次后 free 全部,bitmap 恢复全空闲,可再次 alloc。
//! 3. **不越界**:alloc 返回的索引 < capacity(高位预占 + 兜底校验)。
//! 4. **不 panic**:所有交错下 CAS 循环正常退出。

#![cfg(feature = "loom")]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::thread;

// =============================================================================
// BufferIndexPool — 复制自 iouring.rs(全 Relaxed 内存序)
// =============================================================================

/// fixed buffer 索引分配池(复制自 iouring.rs::BufferIndexPool)。
///
/// 位图语义: `0` = 空闲, `1` = 已占用。超出 `buffer_count` 的高位在构造时
/// 预置为 `1`,防止分配到越界索引。`alloc`/`free` 均为原子操作。
struct BufferIndexPool {
    bitmap: Box<[AtomicU64]>,
    buffer_count: usize,
}

impl BufferIndexPool {
    fn new(buffer_count: usize) -> Self {
        Self {
            bitmap: build_buffer_bitmap(buffer_count),
            buffer_count,
        }
    }

    /// 原子分配一个空闲索引,全部占用时返回 None。
    fn alloc(&self) -> Option<usize> {
        bitmap_alloc_first_free(&self.bitmap, self.buffer_count)
    }

    /// 释放索引使其可被重新分配。idx 越界时静默忽略。
    fn free(&self, idx: usize) {
        if idx >= self.buffer_count {
            return;
        }
        let word_idx = idx / 64;
        let bit = idx % 64;
        self.bitmap[word_idx].fetch_and(!(1u64 << bit), Ordering::Relaxed);
    }

    /// 统计 `[0, buffer_count)` 范围内已占用的位数(观测用,非生产方法)。
    ///
    /// 仅统计有效索引范围,排除 `build_buffer_bitmap` 预占的越界高位
    /// (这些位恒为 1,计入会让"无泄漏"断言的基线非零)。
    fn occupied_count(&self) -> usize {
        let mut count = 0;
        for (word_idx, word) in self.bitmap.iter().enumerate() {
            let val = word.load(Ordering::Relaxed);
            let bits_in_word = (word_idx * 64 + 64).min(self.buffer_count) - word_idx * 64;
            let valid_mask = if bits_in_word >= 64 {
                u64::MAX
            } else {
                (1u64 << bits_in_word) - 1
            };
            count += (val & valid_mask).count_ones() as usize;
        }
        count
    }
}

/// 构造 fixed buffer 分配位图(复制自 iouring.rs::build_buffer_bitmap)。
fn build_buffer_bitmap(buffer_count: usize) -> Box<[AtomicU64]> {
    let words = buffer_count.div_ceil(64);
    (0..words)
        .map(|word_idx| {
            let excess = (word_idx as i64 + 1) * 64 - buffer_count as i64;
            if excess >= 64 {
                AtomicU64::new(0)
            } else if excess > 0 {
                AtomicU64::new((!0u64) << (64 - excess as usize))
            } else {
                AtomicU64::new(0)
            }
        })
        .collect()
}

/// 在多字 AtomicU64 位图上无锁查找并占用第一个空闲位
/// (复制自 iouring.rs::bitmap_alloc_first_free,全 Relaxed 内存序)。
fn bitmap_alloc_first_free(bitmap: &[AtomicU64], buffer_count: usize) -> Option<usize> {
    for (word_idx, word) in bitmap.iter().enumerate() {
        let mut current = word.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                break;
            }
            let bit = (!current).trailing_zeros() as usize;
            let idx = word_idx * 64 + bit;
            if idx >= buffer_count {
                return None;
            }
            let next = current | (1u64 << bit);
            match word.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return Some(idx),
                Err(actual) => current = actual,
            }
        }
    }
    None
}

// =============================================================================
// CompletionSlots 位图分配器 — 复制自 iocp.rs(AcqRel 内存序)
// =============================================================================

/// IOCP 完成 slot 位图分配器(复制自 iocp.rs::CompletionSlots 的位图部分)。
///
/// 与 BufferIndexPool 逻辑同构,但 alloc/release 的 CAS 使用 `AcqRel` 成功序,
/// 配合生产代码中 slot.state 的 Release/Acquire 传递 pending 写入可见性。
struct SlotBitmapAllocator {
    free_bitmap: Box<[AtomicU64]>,
    capacity: usize,
}

impl SlotBitmapAllocator {
    fn new(capacity: usize) -> Self {
        let words = capacity.div_ceil(64);
        let free_bitmap: Box<[AtomicU64]> = (0..words)
            .map(|word_idx| {
                let excess = (word_idx as i64 + 1) * 64 - capacity as i64;
                if excess > 0 {
                    AtomicU64::new((!0u64) << (64 - excess as usize))
                } else {
                    AtomicU64::new(0)
                }
            })
            .collect();
        Self {
            free_bitmap,
            capacity,
        }
    }

    /// 分配一个空闲 slot(复制自 iocp.rs::CompletionSlots::alloc 的位图 CAS 部分)。
    fn alloc(&self) -> Option<usize> {
        for (word_idx, word) in self.free_bitmap.iter().enumerate() {
            let mut current = word.load(Ordering::Relaxed);
            loop {
                if current == u64::MAX {
                    break;
                }
                let bit = (!current).trailing_zeros() as usize;
                let global_slot = word_idx * 64 + bit;
                if global_slot >= self.capacity {
                    return None;
                }
                let new_val = current | (1u64 << bit);
                match word.compare_exchange_weak(
                    current,
                    new_val,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(global_slot),
                    Err(actual) => current = actual,
                }
            }
        }
        None
    }

    /// 释放 slot(复制自 iocp.rs::CompletionSlots::release 的 CAS 循环)。
    fn release(&self, slot_index: usize) {
        let word_idx = slot_index / 64;
        let bit = slot_index % 64;
        let mask = !(1u64 << bit);
        let mut current = self.free_bitmap[word_idx].load(Ordering::Relaxed);
        loop {
            let new_val = current & mask;
            match self.free_bitmap[word_idx].compare_exchange_weak(
                current,
                new_val,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// 统计 `[0, capacity)` 范围内已占用的位数(观测用,非生产方法)。
    ///
    /// 仅统计有效索引范围,排除构造时预占的越界高位。
    fn occupied_count(&self) -> usize {
        let mut count = 0;
        for (word_idx, word) in self.free_bitmap.iter().enumerate() {
            let val = word.load(Ordering::Relaxed);
            let bits_in_word = (word_idx * 64 + 64).min(self.capacity) - word_idx * 64;
            let valid_mask = if bits_in_word >= 64 {
                u64::MAX
            } else {
                (1u64 << bits_in_word) - 1
            };
            count += (val & valid_mask).count_ones() as usize;
        }
        count
    }
}

// =============================================================================
// Loom 模型测试 — BufferIndexPool
// =============================================================================

/// 验证:两个线程各 alloc 一次,索引不重复、不越界。
///
/// capacity=2 迫使两位落在同一 word,最大化 CAS 竞争交错。
/// 全 Relaxed 序下 loom 穷举所有可能的读改写交错,若 CAS 失败兜底
/// (current=actual 重试)有缺陷,会导致重复分配或 panic。
#[test]
fn loom_buffer_pool_no_duplicate_alloc() {
    loom::model(|| {
        let pool = Arc::new(BufferIndexPool::new(2));
        let pool1 = pool.clone();
        let pool2 = pool.clone();

        let h1 = thread::spawn(move || pool1.alloc());
        let h2 = thread::spawn(move || pool2.alloc());

        let a = h1.join().unwrap();
        let b = h2.join().unwrap();

        // 两个线程都应成功分配(capacity=2)
        let idx_a = a.expect("capacity=2 时线程 1 应分配成功");
        let idx_b = b.expect("capacity=2 时线程 2 应分配成功");

        // 核心不变量:不重复
        assert_ne!(idx_a, idx_b, "LOOM FOUND: 并发 alloc 返回重复索引 {idx_a}");
        // 核心不变量:不越界
        assert!(idx_a < 2, "LOOM FOUND: 索引越界 {idx_a}");
        assert!(idx_b < 2, "LOOM FOUND: 索引越界 {idx_b}");
    });
}

/// 验证:并发 alloc 后 free,位图恢复全空闲,无泄漏。
///
/// 线程 1 alloc+free,线程 2 alloc+free。所有交错下最终 occupied_count==0。
/// 全 Relaxed fetch_and 释放 + Relaxed CAS 分配的组合下,若释放位与分配位
/// 存在丢失更新(如 fetch_and 覆盖了并发 CAS 设置的位),会导致位图状态错误。
#[test]
fn loom_buffer_pool_alloc_free_no_leak() {
    loom::model(|| {
        let pool = Arc::new(BufferIndexPool::new(2));
        let pool1 = pool.clone();
        let pool2 = pool.clone();

        let h1 = thread::spawn(move || {
            if let Some(idx) = pool1.alloc() {
                pool1.free(idx);
            }
        });
        let h2 = thread::spawn(move || {
            if let Some(idx) = pool2.alloc() {
                pool2.free(idx);
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();

        // 核心不变量:无泄漏(所有 alloc 的索引都已 free)
        assert_eq!(
            pool.occupied_count(),
            0,
            "LOOM FOUND: 位图泄漏,occupied_count={} 应为 0",
            pool.occupied_count()
        );
    });
}

/// 验证:capacity 耗尽时 alloc 返回 None,不 panic。
///
/// 3 个线程对 capacity=2 的池各 alloc 一次。必有至少一个返回 None。
/// CAS 循环在全满 word(u64::MAX)上必须正确 break,否则死循环或 panic。
#[test]
fn loom_buffer_pool_exhaustion_returns_none() {
    loom::model(|| {
        let pool = Arc::new(BufferIndexPool::new(2));
        let pool1 = pool.clone();
        let pool2 = pool.clone();
        let pool3 = pool.clone();

        let h1 = thread::spawn(move || pool1.alloc());
        let h2 = thread::spawn(move || pool2.alloc());
        let h3 = thread::spawn(move || pool3.alloc());

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        let r3 = h3.join().unwrap();

        // 3 alloc / capacity=2:至少一个 None
        let success_count = [r1.is_some(), r2.is_some(), r3.is_some()]
            .iter()
            .filter(|&&x| x)
            .count();
        assert!(
            success_count <= 2,
            "LOOM FOUND: 超容量分配 success_count={success_count} > 2"
        );

        // 成功分配的索引互不重复
        let mut indices: Vec<usize> = [r1, r2, r3].iter().flatten().copied().collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(
            indices.len(),
            success_count,
            "LOOM FOUND: 并发 alloc 产生重复索引 {:?}",
            indices
        );
    });
}

/// 验证:alloc-free-alloc 序列下索引可复用,状态正确。
///
/// 线程 1 alloc 后立即 free,线程 2 在任意时刻 alloc。
/// 若 free 的 fetch_and(Relaxed) 与 alloc 的 CAS(Relaxed) 存在竞态,
/// 可能导致 free 的位未被后续 alloc 观测到(可见性问题),或位图状态损坏。
///
/// capacity=2 保证两线程的 alloc 都能成功(不因容量竞争返回 None),
/// 聚焦验证 free 后位图状态正确(occupied_count 不超过未 free 的数量)。
#[test]
fn loom_buffer_pool_free_then_realloc() {
    loom::model(|| {
        let pool = Arc::new(BufferIndexPool::new(2));
        let pool1 = pool.clone();
        let pool2 = pool.clone();

        let h1 = thread::spawn(move || {
            // alloc 必成功(capacity=2,最多 2 线程各取 1)
            let idx = pool1.alloc().expect("capacity=2 alloc 应成功");
            pool1.free(idx);
        });
        let h2 = thread::spawn(move || {
            // alloc 必成功;若 h1 已 free,可能复用同一索引
            let idx = pool2.alloc().expect("capacity=2 alloc 应成功");
            // 不释放:观测最终占用状态应为 1(h2 持有,h1 已 free)
            idx
        });

        h1.join().unwrap();
        let h2_idx = h2.join().unwrap();

        // 核心不变量:h1 已 free,h2 持有 1 个索引,occupied_count 必为 1。
        // 若 free 的 fetch_and 与 alloc 的 CAS 竞态导致位图损坏,
        // occupied_count 可能 >1(位未被正确清除)或 0(位被误清)。
        assert_eq!(
            pool.occupied_count(),
            1,
            "LOOM FOUND: free+alloc 竞态导致位图状态错误,h2_idx={},occupied_count={}",
            h2_idx,
            pool.occupied_count()
        );
    });
}

// =============================================================================
// Loom 模型测试 — CompletionSlots 位图分配器
// =============================================================================

/// 验证:两个线程各 alloc 一个 slot,索引不重复、不越界。
///
/// AcqRel 内存序下 CAS 保证 Release 语义,与 iocp 生产代码一致。
#[test]
fn loom_slot_bitmap_no_duplicate_alloc() {
    loom::model(|| {
        let slots = Arc::new(SlotBitmapAllocator::new(2));
        let s1 = slots.clone();
        let s2 = slots.clone();

        let h1 = thread::spawn(move || s1.alloc());
        let h2 = thread::spawn(move || s2.alloc());

        let a = h1.join().unwrap();
        let b = h2.join().unwrap();

        let idx_a = a.expect("capacity=2 时线程 1 应分配成功");
        let idx_b = b.expect("capacity=2 时线程 2 应分配成功");

        assert_ne!(idx_a, idx_b, "LOOM FOUND: 并发 alloc 返回重复 slot {idx_a}");
        assert!(idx_a < 2, "LOOM FOUND: slot 越界 {idx_a}");
        assert!(idx_b < 2, "LOOM FOUND: slot 越界 {idx_b}");
    });
}

/// 验证:并发 alloc + release,位图无泄漏。
///
/// iocp release 用 CAS 循环(AcqRel),与 alloc 的 CAS 循环交错。
/// 验证 release 不会丢失并发的 alloc 设置的位。
#[test]
fn loom_slot_bitmap_alloc_release_no_leak() {
    loom::model(|| {
        let slots = Arc::new(SlotBitmapAllocator::new(2));
        let s1 = slots.clone();
        let s2 = slots.clone();

        let h1 = thread::spawn(move || {
            if let Some(idx) = s1.alloc() {
                s1.release(idx);
            }
        });
        let h2 = thread::spawn(move || {
            if let Some(idx) = s2.alloc() {
                s2.release(idx);
            }
        });

        h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(
            slots.occupied_count(),
            0,
            "LOOM FOUND: slot 位图泄漏,occupied_count={} 应为 0",
            slots.occupied_count()
        );
    });
}

/// 验证:并发 alloc 与 release 的竞态安全性(2 线程版,控制 loom 状态空间)。
///
/// 线程 1 alloc 后 release;线程 2 alloc 后不 release。
/// AcqRel 序下 release 的 CAS 与 alloc 的 CAS 互斥,不会产生
/// "release 覆盖 alloc 设置的位" 或 "alloc 复用未 release 的 slot"。
/// 最终占用数必为 1(线程 1 已 release,线程 2 持有)。
#[test]
fn loom_slot_bitmap_concurrent_alloc_release_same_pool() {
    loom::model(|| {
        let slots = Arc::new(SlotBitmapAllocator::new(2));
        let s1 = slots.clone();
        let s2 = slots.clone();

        // 线程 1:alloc 后立即 release
        let h1 = thread::spawn(move || {
            if let Some(idx) = s1.alloc() {
                s1.release(idx);
            }
        });
        // 线程 2:alloc 后持有不释放
        let h2 = thread::spawn(move || s2.alloc());

        h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // h2 若成功分配,索引必须 < capacity
        if let Some(idx) = r2 {
            assert!(idx < 2, "LOOM FOUND: slot 越界 {idx}");
        }
        // 最终占用数:h1 已 release,h2 若成功则占用 1,否则 0
        let occ = slots.occupied_count();
        assert!(
            occ <= 1,
            "LOOM FOUND: occupied_count={occ} 超预期(h1 已 release)",
        );
    });
}

/// 验证:capacity 耗尽时 alloc 返回 None,release 后可重新 alloc(2 线程版)。
///
/// 线程 1 alloc;线程 2 alloc。capacity=1,两线程竞争同一 slot,
/// 必有一个 Some 一个 None。成功分配的线程 release 后,主线程验证
/// 位图可回收再分配。2 线程控制 loom 状态空间。
#[test]
fn loom_slot_bitmap_exhaustion_and_recycle() {
    loom::model(|| {
        let slots = Arc::new(SlotBitmapAllocator::new(1));
        let s1 = slots.clone();
        let s2 = slots.clone();

        // 两线程竞争 capacity=1 的 slot
        let h1 = thread::spawn(move || s1.alloc());
        let h2 = thread::spawn(move || s2.alloc());

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // capacity=1:恰好一个成功,一个 None
        let success_count = [r1.is_some(), r2.is_some()].iter().filter(|&&x| x).count();
        assert_eq!(
            success_count, 1,
            "LOOM FOUND: capacity=1 下成功分配数应为 1,实际 {success_count}"
        );

        // 释放成功分配的 slot
        for idx in [&r1, &r2].into_iter().flatten() {
            slots.release(*idx);
        }

        // 位图应完全回收
        assert_eq!(
            slots.occupied_count(),
            0,
            "LOOM FOUND: release 后位图未完全回收,occupied_count={}",
            slots.occupied_count()
        );

        // 回收后应能重新分配(可见性正确)
        let re_alloc = slots.alloc();
        assert!(
            re_alloc.is_some(),
            "LOOM FOUND: 位图回收后 alloc 失败(可见性问题)"
        );
    });
}

// =============================================================================
// 跨实现对比:Relaxed vs AcqRel 在 loom 模型下的等价性
// =============================================================================

/// 验证:BufferIndexPool(Relaxed)与 SlotBitmapAllocator(AcqRel)在相同
/// 并发场景下产生一致的不变量(无重复、不越界)。
///
/// loom 的 Relaxed 模型会穷举所有可能的内存可见性交错(比 AcqRel 更宽松),
/// 若 Relaxed 实现的不变量在 loom 下成立,则 AcqRel 实现必然也成立
/// (AcqRel 是 Relaxed 的加强,约束更多交错)。此测试显式验证两者一致性。
#[test]
fn loom_relaxed_and_acqrel_both_safe() {
    loom::model(|| {
        let pool = Arc::new(BufferIndexPool::new(2));
        let slots = Arc::new(SlotBitmapAllocator::new(2));

        // 对 BufferIndexPool(Relaxed)做并发 alloc
        let p1 = pool.clone();
        let p2 = pool.clone();
        let h1 = thread::spawn(move || p1.alloc());
        let h2 = thread::spawn(move || p2.alloc());

        // 对 SlotBitmapAllocator(AcqRel)做并发 alloc
        let s1 = slots.clone();
        let s2 = slots.clone();
        let h3 = thread::spawn(move || s1.alloc());
        let h4 = thread::spawn(move || s2.alloc());

        let bp_a = h1.join().unwrap();
        let bp_b = h2.join().unwrap();
        let sb_a = h3.join().unwrap();
        let sb_b = h4.join().unwrap();

        // 两套实现都满足无重复不变量
        if let (Some(a), Some(b)) = (bp_a, bp_b) {
            assert_ne!(a, b, "LOOM FOUND: BufferIndexPool(Relaxed) 重复分配");
            assert!(a < 2 && b < 2, "LOOM FOUND: BufferIndexPool 越界");
        }
        if let (Some(a), Some(b)) = (sb_a, sb_b) {
            assert_ne!(a, b, "LOOM FOUND: SlotBitmapAllocator(AcqRel) 重复分配");
            assert!(a < 2 && b < 2, "LOOM FOUND: SlotBitmapAllocator 越界");
        }
    });
}
