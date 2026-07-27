//! Windows/类 Unix 进程级 kill + resume 冒烟
//!
//! 子进程真正被 OS 杀掉,父进程用同一落盘文件 + completed 快照 resume,
//! 最终全文件 blake3 与源一致。
//!
//! 入口:
//! - `#[test] os_kill_resume_blake3_matches_source` — 被 nextest 收集
//! - 同二进制 `--child` — 下载 worker(由测试 spawn 并 kill)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tachyon_core::config::{CrashConsistencyMode, IoStrategy};
use tachyon_core::test_harness::harness::{MockProtocol as MockProto, test_config, test_metadata};
use tachyon_core::traits::{Protocol, Verifier};
use tachyon_core::types::DownloadState;
use tachyon_crypto::CpuVerifier;
use tachyon_engine::downloader::{DownloadTask, StorageKind};

const FRAG_SIZE: u64 = 64 * 1024;
const N_FRAGS: u64 = 8;
const TOTAL: u64 = FRAG_SIZE * N_FRAGS;
const MARKER_NAME: &str = "progress.marker";
const PAYLOAD_NAME: &str = "payload.bin";
const OUT_NAME: &str = "download.bin";

fn work_dir() -> PathBuf {
    if let Ok(p) = std::env::var("TACHYON_OS_KILL_WORKDIR") {
        return PathBuf::from(p);
    }
    std::env::temp_dir().join(format!("tachyon-os-kill-{}", std::process::id()))
}

fn make_payload() -> Vec<u8> {
    let mut raw = vec![0u8; TOTAL as usize];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = ((i / FRAG_SIZE as usize) as u8)
            .wrapping_mul(41)
            .wrapping_add((i % 251) as u8);
    }
    raw
}

fn expected_hash(payload: &[u8]) -> String {
    CpuVerifier::blake3().compute_hash(payload).unwrap()
}

fn write_marker(dir: &Path, completed: &[u32], downloaded: u64) {
    let path = dir.join(MARKER_NAME);
    let body = format!(
        "completed={}\ndownloaded={downloaded}\n",
        completed
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let tmp = dir.join(format!("{MARKER_NAME}.tmp"));
    fs::write(&tmp, body).expect("write marker tmp");
    let _ = fs::remove_file(&path);
    fs::rename(&tmp, &path).expect("rename marker");
}

fn read_marker(dir: &Path) -> Option<(Vec<u32>, u64)> {
    let text = fs::read_to_string(dir.join(MARKER_NAME)).ok()?;
    let mut completed = Vec::new();
    let mut downloaded = 0u64;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("completed=") {
            if !rest.is_empty() {
                completed = rest
                    .split(',')
                    .filter_map(|s| s.parse::<u32>().ok())
                    .collect();
            }
        } else if let Some(rest) = line.strip_prefix("downloaded=") {
            downloaded = rest.parse().unwrap_or(0);
        }
    }
    Some((completed, downloaded))
}

async fn open_storage(path: &Path) -> StorageKind {
    StorageKind::open_with_strategy(path, IoStrategy::Standard)
        .await
        .expect("open storage")
}

fn make_task(protocol: Arc<dyn Protocol>, storage: StorageKind) -> DownloadTask {
    let mut task = DownloadTask::new_for_test(
        "http://example.com/os-kill.bin".into(),
        tachyon_core::config::DownloadConfig {
            max_retries: 0,
            verify_checksum: false,
            max_concurrent_fragments: 2,
            crash_consistency_mode: CrashConsistencyMode::EveryFragment,
            ..test_config()
        },
        protocol,
        storage,
    );
    task.set_scheduler_config(tachyon_core::config::SchedulerConfig {
        min_fragment_size: FRAG_SIZE,
        max_fragment_size: FRAG_SIZE,
        ..Default::default()
    });
    task
}

async fn child_download(dir: PathBuf) {
    let payload = fs::read(dir.join(PAYLOAD_NAME)).expect("read payload");
    assert_eq!(payload.len(), TOTAL as usize);

    let meta = test_metadata("os-kill.bin", TOTAL);
    let protocol: Arc<dyn Protocol> = Arc::new(
        MockProto::new(meta)
            .with_default_data(Bytes::from(payload))
            .with_chunk_size(4 * 1024)
            .with_chunk_delay(Duration::from_millis(8)),
    );

    let out_path = dir.join(OUT_NAME);
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&out_path)
            .expect("create out");
        f.set_len(TOTAL).expect("set_len");
        f.sync_all().ok();
        let _ = f.flush();
    }

    let storage = open_storage(&out_path).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let mut task = make_task(protocol, storage);
    task.set_progress_sender(tx);

    task.probe().await.expect("probe");
    let planned = task.plan().expect("plan");
    assert_eq!(planned.len(), N_FRAGS as usize);
    task.prepare_storage().await.expect("prepare");

    let dir_for_marker = dir.clone();
    let marker_task = tokio::spawn(async move {
        let mut completed = Vec::new();
        let mut last_downloaded = 0u64;
        while let Some(ev) = rx.recv().await {
            match ev {
                tachyon_core::FragmentProgress::Chunk {
                    completed: true,
                    fragment_index,
                    fragment_downloaded,
                    ..
                } => {
                    if !completed.contains(&fragment_index) {
                        completed.push(fragment_index);
                        completed.sort_unstable();
                    }
                    last_downloaded = last_downloaded.max(fragment_downloaded);
                    write_marker(&dir_for_marker, &completed, last_downloaded);
                }
                tachyon_core::FragmentProgress::Chunk {
                    completed: false,
                    fragment_downloaded,
                    ..
                } => {
                    last_downloaded = last_downloaded.max(fragment_downloaded);
                    write_marker(&dir_for_marker, &completed, last_downloaded);
                }
                _ => {}
            }
        }
    });

    let _ = task.execute().await;
    drop(task);
    let _ = marker_task.await;
}

async fn parent_resume(dir: &Path, completed: Vec<u32>, payload: &[u8]) {
    let meta = test_metadata("os-kill.bin", TOTAL);
    let protocol: Arc<dyn Protocol> =
        Arc::new(MockProto::new(meta).with_default_data(Bytes::from(payload.to_vec())));

    let out_path = dir.join(OUT_NAME);
    assert!(out_path.exists(), "kill 后下载文件应仍存在");

    let storage = open_storage(&out_path).await;
    let mut task = make_task(protocol, storage);

    task.probe().await.expect("resume probe");
    if !completed.is_empty() {
        task.set_completed_fragments(completed);
    }
    task.plan().expect("resume plan");
    task.prepare_storage().await.expect("resume prepare");
    task.execute().await.expect("resume execute 应成功");
    assert_eq!(task.state(), DownloadState::Completed);

    let on_disk = fs::read(&out_path).expect("read final file");
    assert_eq!(on_disk.len(), TOTAL as usize, "最终文件长度");
    assert_eq!(&on_disk[..], payload, "最终文件字节应等于源");
    let got = expected_hash(&on_disk);
    let exp = expected_hash(payload);
    assert_eq!(got, exp, "OS kill + resume 后全文件 blake3 应等于源");
}

fn run_parent() {
    let dir = work_dir();
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir workdir");

    let payload = make_payload();
    fs::write(dir.join(PAYLOAD_NAME), &payload).expect("write payload");

    // 当前测试二进制作为 child:设 TACHYON_OS_KILL_CHILD 让同 #[test] 走 worker 路径
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(&exe)
        .env("TACHYON_OS_KILL_WORKDIR", &dir)
        .env("TACHYON_OS_KILL_CHILD", "1")
        // 不向 child 传 nextest 过滤参数;裸跑该测试二进制会执行唯一 #[test]
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn child");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen_completed = Vec::new();
    while Instant::now() < deadline {
        if let Some((completed, downloaded)) = read_marker(&dir) {
            if !completed.is_empty() {
                seen_completed = completed;
                eprintln!(
                    "[os_kill_resume] marker ready: completed={seen_completed:?} downloaded={downloaded}"
                );
                break;
            }
            if downloaded > FRAG_SIZE / 4 {
                eprintln!("[os_kill_resume] partial progress={downloaded}, waiting completed…");
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("child 在被 kill 前已退出: {status:?} (可能下载过快或出错)");
        }
        thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !seen_completed.is_empty(),
        "超时未看到任何 completed marker,无法验证 crash 后 resume"
    );

    let _ = child.kill();
    let status = child.wait().expect("wait killed child");
    eprintln!("[os_kill_resume] child killed: {status:?}");

    let completed = read_marker(&dir).map(|(c, _)| c).unwrap_or(seen_completed);
    assert!(
        !completed.is_empty(),
        "kill 后 completed 快照为空,无法 resume 跳过"
    );
    eprintln!("[os_kill_resume] resume with completed={completed:?}");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(parent_resume(&dir, completed, &payload));

    let _ = fs::remove_dir_all(&dir);
    eprintln!("[os_kill_resume] PASS: OS kill + resume blake3 ok");
}

/// nextest/cargo test 入口。
///
/// 若环境变量 `TACHYON_OS_KILL_CHILD=1`,本测试二进制被当作下载 worker 运行并
/// 尽快返回(不走 parent 逻辑)。父测试进程 spawn 本二进制时设置该变量。
#[test]
fn os_kill_resume_blake3_matches_source() {
    if std::env::var_os("TACHYON_OS_KILL_CHILD").is_some() {
        let dir = work_dir();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(child_download(dir));
        return;
    }
    run_parent();
}
