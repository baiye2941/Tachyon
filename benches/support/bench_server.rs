//! 节流流式 HTTP server(bench 专用)
//!
//! 用 hyper 1.x + `http_body_util::StreamBody` 实现逐块流式响应,替代 wiremock
//! 的整包发出(`Full<Bytes>`)。通过 chunk + sleep 模拟真实下行带宽,使 bench 能
//! 验证 BandwidthTracker 采样、动态 RTT 探测、多源聚合等优化候选。
//!
//! # 为何不用 wiremock
//!
//! wiremock 0.6 的 `ResponseTemplate` body 类型硬编码为 `Full<Bytes>`(整包),
//! `Respond` 是同步 trait,`set_delay` 是首字节前固定 sleep。无法做字节级节流
//! 和逐块流式产出。本模块用 hyper streaming server 补齐这一短板。
//!
//! # 节流原理
//!
//! 按 `CHUNK_SIZE`(默认 64KiB)切片响应体,每片后 `sleep(CHUNK_SIZE / bytes_per_sec)`。
//! 非工业级(无 token bucket),但 bench 场景够用:chunk 大小和 sleep 间隔可控,
//! 便于测 BandwidthTracker 的带宽采样周期。

// 本模块所有公开 API 仅被 bench binary 引用,criterion_main! 覆盖了 test harness,
// 编译器无法识别"已被 bench 函数调用"因此报 dead_code。模块级统一放行。
#![allow(dead_code)]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::{StreamExt, stream};
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::sleep;

/// 默认 chunk 大小(64KiB,覆盖 TCP 典型 16-64KiB chunk)
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// bench server 使用的 HTTP 协议模式
///
/// 用于 H2 vs H1 多路复用对比 bench。连接级配置(非请求级),在
/// `ThrottledServer::start_with_protocol` 时确定,影响该 server 所有连接。
#[derive(Clone, Copy, Debug)]
pub enum BenchProtocol {
    /// 自动协商 H1/H2(默认,支持 H2 prior-knowledge)
    Auto,
    /// 仅 HTTP/2(H2 prior-knowledge,不回退 H1)
    Http2Only,
    /// 仅 HTTP/1.1(旧行为,用于对比)
    Http1Only,
}

/// 节流流式 HTTP server 配置
struct ServerConfig {
    /// 模拟文件总大小(字节)
    file_size: u64,
    /// 带宽上限(bytes/sec);0 表示不限速(loopback 全速)
    /// 用 Arc<AtomicU64> 支持运行时动态调整(动态并发度 bench 用)
    bytes_per_sec: Arc<std::sync::atomic::AtomicU64>,
    /// 模拟 RTT(首字节前延迟);0 表示无延迟
    rtt: Duration,
    /// chunk 大小(节流粒度)
    chunk_size: usize,
    /// 连接级握手延迟(每连接 sleep 一次,模拟 TCP+TLS 握手 RTT)
    ///
    /// 在 `serve_connection` 开始处注入(服务任何请求前)。loopback 上 TCP
    /// 握手由内核完成(`accept()` 返回时已完成),此延迟模拟"应用层接受
    /// 连接到开始处理首字节"的等待,等价于高 RTT 网络的握手墙钟成本。
    /// 用于 H2 多路复用 bench:H1 每个并发分片建独立连接各付一次握手,
    /// H2 所有分片复用单连接只付一次握手。
    handshake_rtt: Duration,
}

/// 节流流式 HTTP server
///
/// 启动后绑定 `127.0.0.1:0`(OS 分配端口),处理 HEAD(返回元数据)和
/// GET Range(返回节流 StreamBody)。`uri()` 返回实际 URI 供 HttpClient 使用。
/// `shutdown()` 或 Drop 时关闭:发送 shutdown 信号中断 accept loop,再 abort
/// server task(释放端口)。已 accept 的连接 task 不主动 abort——bench 场景下
/// 迭代已结束,无在途请求;即使 panic 中途退出,runtime drop 会回收残留 task。
///
/// 使用 OS 分配端口而非固定端口:nextest 将 criterion bench 拆分为独立进程并行运行,
/// 固定端口会导致多进程同时绑定同一端口而冲突。OS 分配端口零冲突。
pub struct ThrottledServer {
    uri: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// server loop 的 JoinHandle,shutdown 后 abort 确保端口释放
    join: Option<tokio::task::JoinHandle<()>>,
    /// 已 accept 的连接数(供 H2 多路复用 bench 断言 H1=4 / H2=1)
    accept_count: Arc<AtomicUsize>,
    /// 带宽控制(运行时可调,供动态并发度 bench 模拟带宽变化)
    bandwidth: Arc<std::sync::atomic::AtomicU64>,
}

impl ThrottledServer {
    /// 创建并启动节流 server(OS 分配端口)
    ///
    /// - `file_size`: 模拟文件总大小
    /// - `bytes_per_sec`: 带宽上限(bytes/sec),0 表示不限速
    /// - `rtt_ms`: 模拟 RTT(毫秒),0 表示无延迟
    pub async fn start(file_size: u64, bytes_per_sec: u64, rtt_ms: u64) -> Self {
        Self::start_with_chunk(file_size, bytes_per_sec, rtt_ms, DEFAULT_CHUNK_SIZE).await
    }

    /// 创建并启动 server(自定义 chunk 大小,OS 分配端口)
    pub async fn start_with_chunk(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        chunk_size: usize,
    ) -> Self {
        Self::start_with_protocol(
            file_size,
            bytes_per_sec,
            rtt_ms,
            chunk_size,
            BenchProtocol::Auto,
        )
        .await
    }

    /// 创建并启动 server(指定 HTTP 协议模式,OS 分配端口)
    ///
    /// - `protocol`: HTTP 协议模式(Auto / Http2Only / Http1Only)
    /// - `handshake_rtt`: 连接级握手延迟(每连接 sleep 一次,0=无延迟)
    ///
    /// H2 参数镜像产品客户端配置(`crates/tachyon-protocol/src/http.rs`):
    /// 初始流窗口 1MiB、连接窗口 16MiB、最大帧 1MiB、保活 30s/超时 10s。
    pub async fn start_with_protocol(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        chunk_size: usize,
        protocol: BenchProtocol,
    ) -> Self {
        Self::start_with_handshake(file_size, bytes_per_sec, rtt_ms, 0, chunk_size, protocol).await
    }

    /// 创建并启动 server(指定协议模式 + 连接级握手延迟,OS 分配端口)
    ///
    /// - `handshake_rtt_ms`: 每连接握手延迟(毫秒),0=无延迟
    ///
    /// 用于 H2 多路复用 bench:在 `serve_connection` 开始处注入延迟,模拟高 RTT
    /// 网络的握手墙钟成本。H1 每个并发分片各付一次,H2 所有分片复用单连接只付一次。
    pub async fn start_with_handshake(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        handshake_rtt_ms: u64,
        chunk_size: usize,
        protocol: BenchProtocol,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("端口绑定失败");
        let actual_port = listener.local_addr().expect("获取端口失败").port();
        let config = Arc::new(ServerConfig {
            file_size,
            bytes_per_sec: Arc::new(std::sync::atomic::AtomicU64::new(bytes_per_sec)),
            rtt: Duration::from_millis(rtt_ms),
            chunk_size,
            handshake_rtt: Duration::from_millis(handshake_rtt_ms),
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let accept_count = Arc::new(AtomicUsize::new(0));
        let bandwidth = Arc::clone(&config.bytes_per_sec);

        let uri = format!("http://127.0.0.1:{actual_port}");
        let accept_count_clone = Arc::clone(&accept_count);

        let join = tokio::spawn(async move {
            let mut shutdown_rx = std::pin::pin!(shutdown_rx);
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        let (io, _peer) = match accept_result {
                            Ok(conn) => conn,
                            Err(_) => continue,
                        };
                        // 计数 accept(供 H2 bench 断言连接数)
                        accept_count_clone.fetch_add(1, Ordering::Relaxed);
                        let cfg = Arc::clone(&config);
                        // protocol 是 Copy,可直接 move 进 task(避免借用 listener 作用域)
                        let proto = protocol;
                        tokio::spawn(async move {
                            // 连接级握手延迟:在服务任何请求前注入,模拟高 RTT 网络的
                            // TCP+TLS 握手墙钟成本。loopback 上 TCP 握手由内核完成,
                            // 此 sleep 等价于"应用层接受连接到开始处理首字节"的延迟。
                            if !cfg.handshake_rtt.is_zero() {
                                sleep(cfg.handshake_rtt).await;
                            }
                            let io = TokioIo::new(io);
                            let svc = service_fn(move |req| {
                                let cfg = Arc::clone(&cfg);
                                async move { handle(req, cfg).await }
                            });
                            // auto::Builder 支持 H1/H2 自动协商;按协议模式切换。
                            // H2 参数镜像产品客户端(http.rs):1MiB 流窗口 / 16MiB
                            // 连接窗口 / 1MiB 帧 / 30s 保活 / 10s 超时。
                            // TokioExecutor 在闭包内创建(每次连接独立,无跨连接共享)。
                            // timer(TokioTimer)必需:H2 keepalive PING 需要定时器驱动,
                            // 缺失时 hyper panic("You must supply a timer")。
                            let mut builder = auto::Builder::new(TokioExecutor::new());
                            builder.http1().keep_alive(true).timer(TokioTimer::new());
                            builder
                                .http2()
                                .timer(TokioTimer::new())
                                .initial_stream_window_size(4 * 1024 * 1024)
                                .initial_connection_window_size(16 * 1024 * 1024)
                                .max_frame_size(1 << 20)
                                .keep_alive_interval(Duration::from_secs(30))
                                .keep_alive_timeout(Duration::from_secs(10))
                                .max_concurrent_streams(100);
                            match proto {
                                BenchProtocol::Auto => {}
                                BenchProtocol::Http2Only => {
                                    builder = builder.http2_only();
                                }
                                BenchProtocol::Http1Only => {
                                    builder = builder.http1_only();
                                }
                            }
                            if let Err(e) = builder.serve_connection(io, svc).await {
                                eprintln!("bench server conn error: {e}");
                            }
                        });
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }
            // listener 在此 drop,释放端口
        });

        Self {
            uri,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
            accept_count,
            bandwidth,
        }
    }

    /// 返回 server URI(如 `http://127.0.0.1:54321`,端口由 OS 分配)
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// 运行时调整带宽(bytes/sec),供动态并发度 bench 模拟带宽变化
    pub fn set_bandwidth(&self, bytes_per_sec: u64) {
        self.bandwidth
            .store(bytes_per_sec, std::sync::atomic::Ordering::Relaxed);
    }

    /// 返回已 accept 的连接数(供 H2 bench 断言 H1=4 / H2=1)
    pub fn accept_count(&self) -> usize {
        self.accept_count.load(Ordering::Relaxed)
    }

    /// 重置连接计数器(在每轮 bench 迭代后重置以精确计量单次迭代连接数)
    pub fn reset_accept_count(&self) {
        self.accept_count.store(0, Ordering::Relaxed);
    }

    /// 关闭:发送 shutdown 信号并 abort server task(确保端口释放)
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

impl Drop for ThrottledServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 解析 Range header,返回 (start, end) 闭区间
///
/// 支持 `bytes=start-end` 和 `bytes=start-`(到文件末尾)。
/// 越界或格式错误返回 None。
fn parse_range(range_header: &str, total: u64) -> Option<(u64, u64)> {
    let s = range_header.strip_prefix("bytes=")?;
    let (start_s, end_s) = s.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    let end: u64 = if end_s.is_empty() {
        total.saturating_sub(1)
    } else {
        end_s.parse().ok()?
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end.min(total - 1)))
}

/// 把任意 Body 归一化为 BoxBody<Bytes, std::io::Error>
fn box_body<B>(body: B) -> BoxBody<Bytes, std::io::Error>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    body.map_err(std::io::Error::other).boxed()
}

/// 生成节流流式 body:按 chunk_size 切片,每片后 sleep(节流)
///
/// `data`: 已切好范围的 Bytes
/// `bytes_per_sec`: Arc<AtomicU64> 支持运行时动态调整;0 表示不限速(无 sleep)
/// `rtt`: 首字节前延迟(模拟 TTFB)
/// `chunk_size`: 节流粒度
///
/// 节流时序:第一个 chunk 前 sleep(rtt)(模拟 TTFB),后续每个 chunk 前
/// sleep(chunk_delay)(模拟传输时间)。这是"突发-等待"模式而非平滑流,
/// TTFB = RTT,但 chunk 间有微小空闲(hyper 写缓冲在 sleep 期间空转)。
/// 每 chunk 读取当前 bytes_per_sec(支持运行时动态调整带宽)。
fn throttled_stream(
    data: Bytes,
    bytes_per_sec: Arc<std::sync::atomic::AtomicU64>,
    rtt: Duration,
    chunk_size: usize,
) -> BoxBody<Bytes, std::io::Error> {
    // 首字节 RTT 延迟(在第一个 chunk 前注入)
    let first_chunk_delay = rtt;

    let chunk_size = chunk_size.max(1);

    // 零拷贝切片:slice_ref 共享底层 buffer,避免 copy_from_slice 的逐 chunk 拷贝。
    // data 在 collect 后仍被 chunks 中的 Bytes 引用(引用计数),不会提前 drop。
    let chunks: Vec<Bytes> = data
        .chunks(chunk_size)
        .map(|slice| data.slice_ref(slice))
        .collect();

    let stream = stream::iter(chunks.into_iter().enumerate().map(move |(i, chunk)| {
        let delay = if i == 0 {
            first_chunk_delay
        } else {
            // 每 chunk 读取当前带宽(支持动态调整)
            let bps = bytes_per_sec.load(std::sync::atomic::Ordering::Relaxed);
            (chunk_size as u64)
                .checked_mul(1_000_000)
                .and_then(|micros| micros.checked_div(bps))
                .filter(|_| bps > 0)
                .map_or(Duration::ZERO, Duration::from_micros)
        };
        let frame: Result<Frame<Bytes>, std::io::Error> = Ok(Frame::data(chunk));
        (delay, frame)
    }))
    .then(|(delay, frame)| async move {
        if !delay.is_zero() {
            sleep(delay).await;
        }
        frame
    });

    Box::pin(StreamBody::new(stream))
        .map_err(std::io::Error::other)
        .boxed()
}

/// 生成确定性文件内容(按请求范围分配,range 之外的字节不分配)
///
/// 用确定性填充模式(abs % 251),不依赖随机数。返回的 Bytes 持有完整范围数据。
/// 注意:大范围请求(如完整模式 4MiB)会全量分配内存 + throttled_stream 内再
/// 按 chunk_size 切片复制,峰值内存约为 range 大小的 2 倍。bench 场景可接受。
fn make_file_data(start: u64, end: u64) -> Bytes {
    let len = (end - start + 1) as usize;
    let mut buf = vec![0u8; len];
    // 确定性填充:每 256 字节一个模式(便于哈希校验,不依赖随机)
    for (i, byte) in buf.iter_mut().enumerate() {
        let abs = start as usize + i;
        *byte = (abs % 251) as u8; // 251 是质数,模式周期足够长
    }
    Bytes::from(buf)
}

/// 请求处理器
async fn handle(
    req: Request<Incoming>,
    config: Arc<ServerConfig>,
) -> Result<Response<BoxBody<Bytes, std::io::Error>>, Infallible> {
    let total = config.file_size;

    // HEAD 请求:返回文件元数据 headers(供 HttpClient::probe 使用)
    let method = req.method().clone();
    if method == hyper::Method::HEAD {
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, total.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header("ETag", "\"bench-v1\"")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::LAST_MODIFIED, "Wed, 21 Oct 2026 07:28:00 GMT")
            .body(box_body(Full::new(Bytes::new())))
            .unwrap();
        return Ok(resp);
    }

    // GET 请求:处理 Range
    if method != hyper::Method::GET {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(box_body(Full::new(Bytes::from("method not allowed"))))
            .unwrap());
    }

    // 解析 Range header:区分无 Range(200 全文)、合法 Range(206)、错误 Range(416)
    let (status, start, end) = match req.headers().get(header::RANGE) {
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
        Some(v) => match v.to_str().ok().and_then(|r| parse_range(r, total)) {
            Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
            None => {
                // 格式错误或越界:RFC 7233 要求返回 416 + Content-Range: bytes */{total}
                return Ok(Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(box_body(Full::new(Bytes::new())))
                    .unwrap());
            }
        },
    };

    let body_len = end - start + 1;
    let data = make_file_data(start, end);

    let body = throttled_stream(
        data,
        Arc::clone(&config.bytes_per_sec),
        config.rtt,
        config.chunk_size,
    );

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body_len.to_string());

    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }

    Ok(builder.body(body).unwrap())
}
