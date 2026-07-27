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
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rcgen::CertifiedKey;
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

/// bench server 的 TLS 模式
///
/// 独立于 `BenchProtocol`(H1/H2 选择),使 TLS 与协议矩阵正交:
/// `Plaintext + H2`、`Tls + H1`、`Tls + H2` 等组合均可表达。
/// `Tls` 用 `rcgen` 生成自签证书(localhost),客户端侧需
/// `danger_accept_invalid_certs(true)`(见 `http::with_danger_accept_invalid_certs`)。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsMode {
    #[default]
    Plaintext,
    Tls,
}

/// 源内「慢区间」配置
///
/// 模拟同一个源上某段字节区间劣化(CDN 边缘节点回源、分片落在冷存储等),使
/// straggler(某个分片恰好落在劣化区间)场景可被基准复现。
struct SlowZone {
    /// 慢区间起始偏移(含)
    start: u64,
    /// 慢区间结束偏移(不含)
    end: u64,
    /// 慢区间带宽上限(bytes/sec);0 表示该区间不限速
    ///
    /// 与全局带宽同为 `Arc<AtomicU64>`,可直接交给 `throttled_stream`,
    /// 无需为每个落入慢区间的请求另行分配。
    bytes_per_sec: Arc<std::sync::atomic::AtomicU64>,
}

impl SlowZone {
    /// 判断请求 Range 的起始偏移是否落在慢区间内
    ///
    /// 判据只看 `start`:真实服务端的限速是按连接的,一个连接要么全程快、
    /// 要么全程慢,不会逐字节切换。
    fn covers(&self, start: u64) -> bool {
        start >= self.start && start < self.end
    }
}

/// 包装 Plaintext(TcpStream) 或 Tls(TlsStream<TcpStream>) 两种连接类型,使
/// `serve_connection` 的 io 参数有统一类型。Rust 不允许多个非 auto trait 在
/// `dyn` 上组合(`Box<dyn AsyncRead + AsyncWrite + Send>` 无效),故用 enum。
/// TlsStream 体积显著大于 TcpStream,用 Box 消除 enum 变体大小差异。
#[allow(clippy::large_enum_variant)]
enum BenchIo {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
}

impl tokio::io::AsyncRead for BenchIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            BenchIo::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            BenchIo::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for BenchIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            BenchIo::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            BenchIo::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            BenchIo::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            BenchIo::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            BenchIo::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            BenchIo::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// 节流流式 HTTP server 配置
struct ServerConfig {
    /// 模拟文件总大小(字节)
    file_size: u64,
    /// 带宽上限(bytes/sec);0 表示不限速(loopback 全速)
    /// 用 Arc<AtomicU64> 支持运行时动态调整(动态并发度 bench 用)
    bytes_per_sec: Arc<std::sync::atomic::AtomicU64>,
    /// 源内慢区间;None 表示全局均匀限速(既有行为)
    slow_zone: Option<SlowZone>,
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
    /// TLS 模式(默认 Plaintext)。`Tls` 时在 `serve_connection` 前包一层
    /// `TlsAcceptor::accept`,用 `rcgen` 生成的自签证书。
    tls: TlsMode,
    /// chunk 丢包率 [0.0, 1.0],0=不丢。`throttled_stream` 按概率丢弃 chunk
    /// 使 stream 提前结束,reqwest 收到不完整 body 报 connection error,
    /// 触发 downloader 分片重试/续传路径(模拟真实 TCP 中断 + 重连)。
    loss_rate: f64,
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
        Self::start_inner(
            file_size,
            bytes_per_sec,
            rtt_ms,
            handshake_rtt_ms,
            chunk_size,
            protocol,
            None,
            TlsMode::Plaintext,
            0.0,
        )
        .await
    }

    /// 创建并启动 server(带源内慢区间,OS 分配端口)
    ///
    /// - `slow_zone`: `(zone_start, zone_end_exclusive, slow_bps)`;`None` 等价于 `start()`
    ///
    /// 请求 Range 的**起始偏移**落在 `[zone_start, zone_end)` 内时,整个响应用
    /// `slow_bps` 限速,否则用全局 `bytes_per_sec`。判据只看 start:真实服务端
    /// 限速按连接生效,一个连接要么全程快、要么全程慢——这也正确模拟了「某个
    /// 分片恰好落在劣化区间」的 straggler 场景。
    ///
    /// chunk 大小固定为 `DEFAULT_CHUNK_SIZE`,与 `start()` 一致。
    pub async fn start_with_slow_zone(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        slow_zone: Option<(u64, u64, u64)>,
    ) -> Self {
        Self::start_inner(
            file_size,
            bytes_per_sec,
            rtt_ms,
            0,
            DEFAULT_CHUNK_SIZE,
            BenchProtocol::Auto,
            slow_zone,
            TlsMode::Plaintext,
            0.0,
        )
        .await
    }

    /// 全参数构造入口:支持 slow_zone + handshake_rtt + protocol + TLS + loss_rate
    ///
    /// 这是 P0-2 真实基线的主入口,主源与镜像源均用此构造以表达异构配置。
    /// TLS 自签证书由 `rcgen::generate_simple_self_signed_cert(["localhost"])` 生成。
    ///
    /// 9 个参数均不可折叠:`(file_size, bps, rtt)` 描述源基本属性,
    /// `(handshake_rtt, chunk_size, protocol)` 描述连接层,`(slow_zone, tls, loss_rate)`
    /// 描述故障注入。bench 场景需正交矩阵,故参数全部展开(非生产 API,不引 builder)。
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_tls_and_loss(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        handshake_rtt_ms: u64,
        chunk_size: usize,
        protocol: BenchProtocol,
        slow_zone: Option<(u64, u64, u64)>,
        tls: TlsMode,
        loss_rate: f64,
    ) -> Self {
        Self::start_inner(
            file_size,
            bytes_per_sec,
            rtt_ms,
            handshake_rtt_ms,
            chunk_size,
            protocol,
            slow_zone,
            tls,
            loss_rate,
        )
        .await
    }

    /// 全参数构造入口(私有):其余 `start_*` 均委托至此
    ///
    /// 参数语义见 `start_with_tls_and_loss`,此处仅是私有实现底座,参数列表与之
    /// 一致以避免在委托链中再展开/折叠参数(徒增间接层)。
    #[allow(clippy::too_many_arguments)]
    async fn start_inner(
        file_size: u64,
        bytes_per_sec: u64,
        rtt_ms: u64,
        handshake_rtt_ms: u64,
        chunk_size: usize,
        protocol: BenchProtocol,
        slow_zone: Option<(u64, u64, u64)>,
        tls: TlsMode,
        loss_rate: f64,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("端口绑定失败");
        let actual_port = listener.local_addr().expect("获取端口失败").port();
        let config = Arc::new(ServerConfig {
            file_size,
            bytes_per_sec: Arc::new(std::sync::atomic::AtomicU64::new(bytes_per_sec)),
            slow_zone: slow_zone.map(|(start, end, slow_bps)| SlowZone {
                start,
                end,
                bytes_per_sec: Arc::new(std::sync::atomic::AtomicU64::new(slow_bps)),
            }),
            rtt: Duration::from_millis(rtt_ms),
            chunk_size,
            handshake_rtt: Duration::from_millis(handshake_rtt_ms),
            tls,
            loss_rate,
        });
        // TLS 自签证书(Tls 模式):用 rcgen 生成 localhost 自签证书 + 私钥,
        // 构造 rustls ServerConfig。证书链仅 bench 用,客户端侧需
        // danger_accept_invalid_certs(true) 跳过校验。
        // rustls 0.23 需显式安装 CryptoProvider(进程级,幂等)。
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls_acceptor = if tls == TlsMode::Tls {
            let CertifiedKey { cert, key_pair } =
                rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
                    .expect("rcgen 生成自签证书失败");
            let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
            let key_der = rustls::pki_types::PrivateKeyDer::from(
                rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()),
            );
            let server_cfg = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key_der)
                .expect("rustls ServerConfig 构造失败");
            Some(Arc::new(tokio_rustls::TlsAcceptor::from(
                std::sync::Arc::new(server_cfg),
            )))
        } else {
            None
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let accept_count = Arc::new(AtomicUsize::new(0));
        let bandwidth = Arc::clone(&config.bytes_per_sec);

        // URI scheme 据 TLS 模式切换(客户端构造 https:// URL 走 rustls)
        let scheme = if tls == TlsMode::Tls { "https" } else { "http" };
        let uri = format!("{scheme}://127.0.0.1:{actual_port}");
        let accept_count_clone = Arc::clone(&accept_count);

        let join = tokio::spawn(async move {
            let mut shutdown_rx = std::pin::pin!(shutdown_rx);
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        let (raw_io, _peer) = match accept_result {
                            Ok(conn) => conn,
                            Err(_) => continue,
                        };
                        // 计数 accept(供 H2 bench 断言连接数)
                        accept_count_clone.fetch_add(1, Ordering::Relaxed);
                        let cfg = Arc::clone(&config);
                        // protocol/tls/acceptor 是 Copy 或 Arc,可直接 move 进 task
                        let proto = protocol;
                        let acceptor = tls_acceptor.clone();
                        tokio::spawn(async move {
                            // 连接级握手延迟:在服务任何请求前注入,模拟高 RTT 网络的
                            // TCP+TLS 握手墙钟成本。loopback 上 TCP 握手由内核完成,
                            // 此 sleep 等价于"应用层接受连接到开始处理首字节"的延迟。
                            if !cfg.handshake_rtt.is_zero() {
                                sleep(cfg.handshake_rtt).await;
                            }
                            // TLS 模式:在应用层 serve_connection 前包一层 TlsAcceptor。
                            // 失败视为连接断开(等同真 TLS 握手失败),abort 此 task。
                            // 用 BenchIo enum 统一 Plain / Tls 两类连接类型。
                            let io: BenchIo = if let Some(acceptor) = acceptor {
                                match acceptor.accept(raw_io).await {
                                    Ok(tls_io) => BenchIo::Tls(Box::new(tls_io)),
                                    Err(e) => {
                                        eprintln!("bench server TLS accept 失败: {e}");
                                        return;
                                    }
                                }
                            } else {
                                BenchIo::Plain(raw_io)
                            };
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
/// `loss_rate`: chunk 丢包率 [0.0, 1.0],0=不丢。按概率在某个 chunk 前 stream 提前
///   `None` 结束,使 HTTP body 截断 → reqwest 报 "error decoding response body"
///   → 触发 downloader 分片重试/续传路径(模拟真实 TCP 中断 + 重连)。
///   首个 chunk 不丢(避免零字节响应被误判为协议错误而非传输中断)。
///
/// 节流时序:第一个 chunk 前 sleep(rtt)(模拟 TTFB),后续每个 chunk 前
/// sleep(chunk_delay)(模拟传输时间)。这是"突发-等待"模式而非平滑流,
/// TTFB = RTT,但 chunk 间有微小空闲(hyper 写缓冲在 sleep 期间空转)。
/// 每 chunk 读取当前 bytes_per_sec(支持运行时动态调整带宽)。
#[allow(clippy::too_many_arguments)]
fn throttled_stream(
    data: Bytes,
    bytes_per_sec: Arc<std::sync::atomic::AtomicU64>,
    rtt: Duration,
    chunk_size: usize,
    loss_rate: f64,
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

    // 丢包用确定性 RNG(种子固定),保证 bench 可复现。loss_rate=0 时 RNG 不构造,
    // 走纯节流路径(既有行为,零额外开销)。
    const BENCH_LOSS_SEED: u64 = 0xBEEF_C0FFEE_u64;
    let rng = if loss_rate > 0.0 {
        Some(StdRng::seed_from_u64(BENCH_LOSS_SEED))
    } else {
        None
    };
    let rng = Arc::new(tokio::sync::Mutex::new(rng));

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
        (i, delay, frame)
    }))
    .then(move |(i, delay, frame)| {
        let rng = Arc::clone(&rng);
        async move {
            if !delay.is_zero() {
                sleep(delay).await;
            }
            // 丢包判定:首个 chunk(i==0)不丢(避免零字节响应歧义);
            // 后续 chunk 按 loss_rate 概率丢,丢则 stream 提前 None。
            if loss_rate > 0.0 && i > 0 {
                let drop = {
                    let mut g = rng.lock().await;
                    g.as_mut()
                        .map(|r| r.random::<f64>() < loss_rate)
                        .unwrap_or(false)
                };
                if drop {
                    // 返回 Err 终止 body:hyper 会关闭流,reqwest 收到不完整 body
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "bench server 模拟丢包:chunk drop",
                    ));
                }
            }
            frame
        }
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

    // 选速率:起始偏移落在慢区间内则整个响应降速,否则用全局带宽。
    // 只在此处分流,`throttled_stream` 的节流算法保持不变。
    let rate = config
        .slow_zone
        .as_ref()
        .filter(|zone| zone.covers(start))
        .map_or_else(
            || Arc::clone(&config.bytes_per_sec),
            |zone| Arc::clone(&zone.bytes_per_sec),
        );

    let body = throttled_stream(data, rate, config.rtt, config.chunk_size, config.loss_rate);

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
