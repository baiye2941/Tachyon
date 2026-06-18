// Tachyon 应用入口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 使用 mimalloc 替代系统默认分配器(glibc malloc / Windows HeapAlloc)
// 高并发下载场景下,mimalloc 的线程缓存和 size class 优化可减少 10-30% 分配开销,
// 降低内存碎片,提升多线程分配吞吐量。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    tachyon_app_lib::run();
}
