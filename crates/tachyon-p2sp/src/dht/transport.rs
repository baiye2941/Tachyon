//! DHT UDP 传输层: RPC 请求-响应、迭代查找、分布式存储

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::{RwLock, oneshot};

use super::kademlia::{KademliaDht, key_to_node_id};
use super::message::KademliaMessage;
use super::node::{ALPHA, DhtNode, K_BUCKET_SIZE, NUM_BUCKETS, NodeId, xor_distance};

/// Bucket Refresh 检查间隔 (Kademlia 推荐 15 分钟)
const BUCKET_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Bucket 不活跃阈值: 超过此时间无活动的 bucket 需要刷新
const BUCKET_STALE_THRESHOLD: Duration = Duration::from_secs(900);

// ============================================================
// TransportError
// ============================================================

/// DHT 网络传输错误
#[derive(Debug)]
pub enum TransportError {
    /// 消息序列化或反序列化失败
    Serialization(String),
    /// UDP I/O 错误
    Io(std::io::Error),
    /// RPC 请求超时(默认 5 秒)
    Timeout,
    /// 传输层已关闭
    Shutdown,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Timeout => write!(f, "RPC request timed out"),
            Self::Shutdown => write!(f, "transport is shut down"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================
// DhtTransport — UDP RPC 网络层
// ============================================================

/// RPC 请求 ID(单调递增计数器,用于匹配请求与响应)
type RequestId = u64;

/// 默认 RPC 超时时间(5 秒)
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

// ============================================================
// 线格式编解码 (A-11: postcard 二进制格式)
// ============================================================

/// 将 KademliaMessage 编码为 postcard 二进制字节
fn wire_encode(msg: &KademliaMessage) -> Result<Vec<u8>, TransportError> {
    postcard::to_allocvec(msg).map_err(|e| TransportError::Serialization(e.to_string()))
}

/// 从 postcard 二进制字节解码 KademliaMessage
fn wire_decode(bytes: &[u8]) -> Result<KademliaMessage, TransportError> {
    postcard::from_bytes(bytes).map_err(|e| TransportError::Serialization(e.to_string()))
}

/// DHT UDP 传输层
///
/// 在 `KademliaDht` 之上提供基于 UDP 的 RPC 网络能力:
/// - 消息 postcard 二进制序列化/反序列化 (A-11)
/// - 请求-响应关联(通过 8 字节 request ID 头)
/// - 后台接收循环与消息路由
/// - 迭代式分布式查找(FIND_NODE / FIND_VALUE / STORE)
/// - 周期性 Bucket Refresh (A-10)
///
/// # 线格式
///
/// ```text
/// [request_id: u64 big-endian (8 bytes)][postcard binary payload]
/// ```
///
/// 相比 JSON, postcard 二进制编码可减少 50-70% 的包体积,
/// 序列化/反序列化速度提升 5-10x,且避免 UDP 分片丢失风险。
pub struct DhtTransport {
    socket: Arc<UdpSocket>,
    dht: Arc<RwLock<KademliaDht>>,
    pending_requests: Arc<DashMap<RequestId, oneshot::Sender<KademliaMessage>>>,
    running: Arc<AtomicBool>,
    next_request_id: AtomicU64,
}

impl DhtTransport {
    /// 绑定到指定地址的 UDP socket,创建传输层实例
    ///
    /// 使用 `addr = "127.0.0.1:0"` 可由操作系统自动分配可用端口。
    pub async fn bind(addr: &str, dht: Arc<RwLock<KademliaDht>>) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: Arc::new(socket),
            dht,
            pending_requests: Arc::new(DashMap::new()),
            running: Arc::new(AtomicBool::new(false)),
            next_request_id: AtomicU64::new(1),
        })
    }

    /// 获取本地 socket 地址
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        Ok(self.socket.local_addr()?)
    }

    /// 获取本节点 ID
    pub async fn local_id(&self) -> NodeId {
        *self.dht.read().await.local_id()
    }

    /// 获取底层 DHT 的 Arc 引用
    pub fn dht(&self) -> &Arc<RwLock<KademliaDht>> {
        &self.dht
    }

    /// 启动后台接收循环
    ///
    /// 在独立的 tokio task 中持续接收 UDP 报文:
    /// - **响应消息**: 匹配到 pending request,通过 oneshot channel 返回给调用者
    /// - **请求消息**: 调用 `process_incoming` 生成响应并回送
    ///
    /// 调用 [`shutdown()`](Self::shutdown) 可终止循环。
    pub fn start_recv_loop(&self) {
        self.running.store(true, Ordering::SeqCst);
        let socket = self.socket.clone();
        let dht = self.dht.clone();
        let pending = self.pending_requests.clone();
        let running = self.running.clone();
        tokio::spawn(Self::recv_loop_inner(socket, dht, pending, running));
    }

    /// 关闭传输层,停止接收循环
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// 检查传输层是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 启动周期性 Bucket Refresh 循环 (A-10)
    ///
    /// 每 60 秒检查一次路由表,对超过 15 分钟无活动的 bucket
    /// 发起 `iterative_find_node` 查找随机 ID,以发现新节点并填充路由表。
    ///
    /// Kademlia 规范要求此机制以确保低活跃度桶不会因节点离线而变空。
    /// 调用 [`shutdown()`](Self::shutdown) 可终止循环。
    pub fn start_bucket_refresh_loop(&self) {
        let dht = self.dht.clone();
        let running = self.running.clone();
        tokio::spawn(Self::bucket_refresh_loop(dht, running));
    }

    /// Bucket Refresh 循环内部实现
    async fn bucket_refresh_loop(dht: Arc<RwLock<KademliaDht>>, running: Arc<AtomicBool>) {
        while running.load(Ordering::SeqCst) {
            tokio::time::sleep(BUCKET_REFRESH_INTERVAL).await;

            if !running.load(Ordering::SeqCst) {
                break;
            }

            // 收集需要刷新的 bucket 索引
            let stale_buckets: Vec<usize> = {
                let dht_guard = dht.read().await;
                let rt = dht_guard.routing_table();
                let now = std::time::Instant::now();
                (0..NUM_BUCKETS)
                    .filter(|&i| {
                        rt.bucket(i).map_or(false, |b| {
                            !b.is_empty()
                                && now.duration_since(b.last_activity()) > BUCKET_STALE_THRESHOLD
                        })
                    })
                    .collect()
            };

            if stale_buckets.is_empty() {
                continue;
            }

            tracing::debug!(
                count = stale_buckets.len(),
                "Bucket Refresh: {} 个不活跃 bucket 需要刷新",
                stale_buckets.len()
            );

            for bucket_idx in stale_buckets {
                if !running.load(Ordering::SeqCst) {
                    break;
                }

                // 获取本地候选节点并刷新 bucket 的时间戳
                {
                    let mut dht_guard = dht.write().await;
                    let candidates = dht_guard.refresh_bucket(bucket_idx);
                    // 刷新 bucket 活动时间,避免短时间内重复刷新
                    if let Some(bucket) = dht_guard.routing_table_mut().bucket_mut(bucket_idx) {
                        bucket.touch_activity();
                    }
                    drop(dht_guard);

                    if candidates.is_empty() {
                        continue;
                    }

                    tracing::trace!(
                        bucket = bucket_idx,
                        candidates = candidates.len(),
                        "Bucket Refresh: 已刷新 bucket,发现 {} 个候选节点",
                        candidates.len()
                    );
                }
            }
        }
    }

    // ----------------------------------------------------------
    // 内部: 接收循环
    // ----------------------------------------------------------

    async fn recv_loop_inner(
        socket: Arc<UdpSocket>,
        dht: Arc<RwLock<KademliaDht>>,
        pending: Arc<DashMap<RequestId, oneshot::Sender<KademliaMessage>>>,
        running: Arc<AtomicBool>,
    ) {
        // S-13: 速率限制参数 — 防止 DHT 被利用为 DDoS 反射器
        const MAX_MESSAGE_SIZE: usize = 8192; // 8KB,防止超大报文
        const MAX_REQUESTS_PER_SEC: u32 = 100; // 全局入站请求上限

        let mut buf = [0u8; 65535];
        let mut window_start = std::time::Instant::now();
        let mut request_count: u32 = 0;

        while running.load(Ordering::SeqCst) {
            let (len, src_addr) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "UDP recv_from failed");
                    continue;
                }
            };
            if len < 8 {
                continue; // 报文过短,无法包含 request ID
            }

            // 反放大: 拒绝超大报文
            if len > MAX_MESSAGE_SIZE {
                tracing::debug!(
                    len = len,
                    addr = %src_addr,
                    "DHT 速率限制: 报文超过 {MAX_MESSAGE_SIZE} 字节,丢弃"
                );
                continue;
            }

            // 解析 request ID(前 8 字节,大端序)
            let request_id = u64::from_be_bytes(buf[..8].try_into().unwrap());

            // 解析 JSON 消息体
            let msg: KademliaMessage = match wire_decode(&buf[8..len]) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to deserialize message");
                    continue;
                }
            };

            // 尝试匹配 pending request(响应消息 — 不计入速率限制)
            if let Some((_, sender)) = pending.remove(&request_id) {
                let _ = sender.send(msg);
                continue;
            }

            // 速率限制: 仅对入站请求计数(响应已在上方处理)
            let now = std::time::Instant::now();
            if now.duration_since(window_start) >= Duration::from_secs(1) {
                window_start = now;
                request_count = 0;
            }
            request_count += 1;
            if request_count > MAX_REQUESTS_PER_SEC {
                tracing::debug!(
                    addr = %src_addr,
                    count = request_count,
                    "DHT 速率限制: 入站请求超过 {MAX_REQUESTS_PER_SEC}/s,丢弃"
                );
                continue;
            }

            // 未匹配:视为入站请求,生成响应并回送
            if let Some(response) = Self::process_incoming(&dht, &msg, src_addr).await {
                let payload = match wire_encode(&response) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize response");
                        continue;
                    }
                };
                let mut packet = Vec::with_capacity(8 + payload.len());
                packet.extend_from_slice(&request_id.to_be_bytes());
                packet.extend_from_slice(&payload);
                if let Err(e) = socket.send_to(&packet, src_addr).await {
                    tracing::warn!(error = %e, addr = %src_addr, "failed to send response");
                }
            }
        }
    }

    // ----------------------------------------------------------
    // 内部: 入站消息处理
    // ----------------------------------------------------------

    /// 处理入站 RPC 请求,返回需要回送的响应(若需要)
    ///
    /// - 请求类消息(Ping / FindNode / FindValue / Store):更新路由表 + 生成响应
    /// - 响应类消息(Pong / FindNodeResponse / FindValueResponse):仅更新路由表,不生成响应
    async fn process_incoming(
        dht: &Arc<RwLock<KademliaDht>>,
        msg: &KademliaMessage,
        src_addr: std::net::SocketAddr,
    ) -> Option<KademliaMessage> {
        let mut dht_guard = dht.write().await;

        // 更新路由表中已知发送方的 last_seen
        dht_guard.handle_message(msg);

        match msg {
            KademliaMessage::Ping { .. } => Some(dht_guard.make_pong()),

            KademliaMessage::FindNode { target, .. } => {
                let nodes = dht_guard.handle_find_node(target);
                Some(dht_guard.make_find_node_response(nodes))
            }

            KademliaMessage::FindValue { key, .. } => {
                let (value, nodes) = dht_guard.find_value_local(key);
                Some(KademliaMessage::FindValueResponse {
                    sender_id: *dht_guard.local_id(),
                    value,
                    nodes,
                })
            }

            KademliaMessage::Store { key, value, .. } => {
                dht_guard.store(key.clone(), value.clone());
                // 将发送方添加到路由表(Ping 验证通过)
                let sender_id = match msg {
                    KademliaMessage::Store { sender_id, .. } => *sender_id,
                    _ => unreachable!(),
                };
                dht_guard.add_node(DhtNode::new(sender_id, src_addr.to_string()));
                None
            }

            // 响应类消息:已在 handle_message 中刷新路由表,无需生成响应
            _ => None,
        }
    }

    // ----------------------------------------------------------
    // 核心: 发送 RPC 请求并等待响应
    // ----------------------------------------------------------

    /// 发送 RPC 请求到目标地址并等待响应
    ///
    /// 1. 分配唯一的 request ID
    /// 2. 注册 oneshot channel 到 pending map
    /// 3. 序列化消息并通过 UDP 发送(前缀 8 字节 request ID)
    /// 4. 等待响应(超时 5 秒)
    ///
    /// # 错误
    ///
    /// - `TransportError::Shutdown`: 传输层已关闭
    /// - `TransportError::Serialization`: 消息序列化失败
    /// - `TransportError::Io`: UDP 发送失败
    /// - `TransportError::Timeout`: 5 秒内未收到响应
    pub async fn send_rpc(
        &self,
        target_addr: &str,
        message: &KademliaMessage,
    ) -> Result<KademliaMessage, TransportError> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(TransportError::Shutdown);
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(request_id, tx);

        let payload = wire_encode(message)?;

        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&request_id.to_be_bytes());
        packet.extend_from_slice(&payload);

        self.socket.send_to(&packet, target_addr).await?;

        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                self.pending_requests.remove(&request_id);
                Err(TransportError::Shutdown)
            }
            Err(_) => {
                self.pending_requests.remove(&request_id);
                Err(TransportError::Timeout)
            }
        }
    }

    // ----------------------------------------------------------
    // 高层 RPC 便捷方法
    // ----------------------------------------------------------

    /// 发送 Ping RPC,成功收到 Pong 则返回 `true`
    pub async fn ping(&self, addr: &str) -> Result<bool, TransportError> {
        let msg = {
            let dht = self.dht.read().await;
            dht.make_ping()
        };
        let response = self.send_rpc(addr, &msg).await?;
        Ok(matches!(response, KademliaMessage::Pong { .. }))
    }

    /// 发送 FindNode RPC,返回目标节点已知的最近节点列表
    pub async fn find_node_rpc(
        &self,
        addr: &str,
        target: &NodeId,
    ) -> Result<Vec<DhtNode>, TransportError> {
        let msg = {
            let dht = self.dht.read().await;
            dht.make_find_node(*target)
        };
        let response = self.send_rpc(addr, &msg).await?;
        match response {
            KademliaMessage::FindNodeResponse { nodes, .. } => Ok(nodes),
            _ => Ok(Vec::new()),
        }
    }

    /// 发送 FindValue RPC,返回 (值, 最近节点列表)
    pub async fn find_value_rpc(
        &self,
        addr: &str,
        key: &[u8],
    ) -> Result<(Option<Vec<u8>>, Vec<DhtNode>), TransportError> {
        let msg = KademliaMessage::FindValue {
            sender_id: self.local_id().await,
            key: key.to_vec(),
        };
        let response = self.send_rpc(addr, &msg).await?;
        match response {
            KademliaMessage::FindValueResponse { value, nodes, .. } => Ok((value, nodes)),
            _ => Ok((None, Vec::new())),
        }
    }

    /// 发送 Store RPC(单向,不等待响应)
    pub async fn store_rpc(
        &self,
        addr: &str,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), TransportError> {
        let msg = KademliaMessage::Store {
            sender_id: self.local_id().await,
            key: key.to_vec(),
            value: value.to_vec(),
        };
        let payload = wire_encode(&msg)?;

        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&request_id.to_be_bytes());
        packet.extend_from_slice(&payload);

        self.socket.send_to(&packet, addr).await?;
        Ok(())
    }

    // ----------------------------------------------------------
    // 迭代式分布式查找
    // ----------------------------------------------------------

    /// 迭代查找距离 target 最近的 k 个节点
    ///
    /// 实现 Kademlia 标准迭代查询:
    /// 1. 从本地路由表取初始候选集
    /// 2. 每轮并发向 `ALPHA`(3) 个最近未查询节点发送 FindNode
    /// 3. 合并新发现的节点,按 XOR 距离排序
    /// 4. 当候选集不再收敛时终止
    pub async fn iterative_find_node(
        &self,
        target: &NodeId,
    ) -> Result<Vec<DhtNode>, TransportError> {
        let mut known: Vec<DhtNode> = {
            let dht = self.dht.read().await;
            dht.routing_table().find_closest(target, K_BUCKET_SIZE)
        };

        if known.is_empty() {
            return Ok(Vec::new());
        }

        let mut queried_ids: std::collections::HashSet<NodeId> = std::collections::HashSet::new();

        loop {
            // 按 XOR 距离排序,选择最多 ALPHA 个未查询节点
            known.sort_by_cached_key(|n| xor_distance(&n.id, target));
            let targets: Vec<DhtNode> = known
                .iter()
                .filter(|n| !queried_ids.contains(&n.id))
                .take(ALPHA)
                .cloned()
                .collect();

            if targets.is_empty() {
                break; // 所有已知节点均已查询,查找收敛
            }

            for t in &targets {
                queried_ids.insert(t.id);
            }

            // 并发 RPC 查询
            let mut handles = Vec::new();
            for node in targets {
                let target_copy = *target;
                handles.push(tokio::spawn({
                    let transport_socket = self.socket.clone();
                    let dht_ref = self.dht.clone();
                    let pending_ref = self.pending_requests.clone();
                    let next_id = &self.next_request_id;
                    let addr = node.addr.clone();
                    let req_id = next_id.fetch_add(1, Ordering::SeqCst);
                    async move {
                        // 构造 FindNode 消息
                        let msg = {
                            let dht = dht_ref.read().await;
                            dht.make_find_node(target_copy)
                        };
                        let payload = match wire_encode(&msg) {
                            Ok(p) => p,
                            Err(_) => return Vec::new(),
                        };
                        let (tx, rx) = oneshot::channel();
                        // 使用临时 pending map
                        let temp_pending: Arc<
                            DashMap<RequestId, oneshot::Sender<KademliaMessage>>,
                        > = pending_ref;
                        temp_pending.insert(req_id, tx);
                        let mut packet = Vec::with_capacity(8 + payload.len());
                        packet.extend_from_slice(&req_id.to_be_bytes());
                        packet.extend_from_slice(&payload);
                        if transport_socket.send_to(&packet, &addr).await.is_err() {
                            temp_pending.remove(&req_id);
                            return Vec::new();
                        }
                        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
                            Ok(Ok(KademliaMessage::FindNodeResponse { nodes, .. })) => nodes,
                            _ => {
                                temp_pending.remove(&req_id);
                                Vec::new()
                            }
                        }
                    }
                }));
            }

            let mut new_found = false;
            for handle in handles {
                if let Ok(nodes) = handle.await {
                    for node in nodes {
                        if !queried_ids.contains(&node.id) && !known.iter().any(|n| n.id == node.id)
                        {
                            known.push(node);
                            new_found = true;
                        }
                    }
                }
            }

            if !new_found {
                break;
            }
        }

        // 按距离排序并截取前 k 个
        known.sort_by_cached_key(|n| xor_distance(&n.id, target));
        known.truncate(K_BUCKET_SIZE);
        Ok(known)
    }

    /// 迭代查找并获取值
    ///
    /// 1. 执行迭代查找定位存储了该 key 的节点
    /// 2. 向候选节点逐个发送 FindValue 直到获取到值
    pub async fn distributed_find_value(
        &self,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, TransportError> {
        let key_node_id = key_to_node_id(key);

        // Phase 1: 迭代查找近距离节点
        let closest = self.iterative_find_node(&key_node_id).await?;

        // Phase 2: 向候选节点发送 FindValue 请求
        for node in &closest {
            match self.find_value_rpc(&node.addr, key).await {
                Ok((Some(value), _)) => return Ok(Some(value)),
                Ok((None, _)) => continue,
                Err(_) => continue,
            }
        }

        Ok(None)
    }

    /// 分布式存储:将键值对复制到最近的 k 个节点
    ///
    /// 1. 通过迭代查找定位最近的 k 个节点
    /// 2. 并发向这些节点发送 Store RPC
    /// 3. 同时存储到本地
    ///
    /// 返回成功存储的节点数。
    pub async fn distributed_store(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<usize, TransportError> {
        let key_node_id = key_to_node_id(&key);

        // 查找最近的 k 个节点
        let closest = self.iterative_find_node(&key_node_id).await?;

        // 并发 Store 到远程节点
        let mut handles = Vec::new();
        for node in &closest {
            let key = key.clone();
            let value = value.clone();
            let addr = node.addr.clone();
            handles.push(tokio::spawn({
                let socket = self.socket.clone();
                let next_id = &self.next_request_id;
                let sender_id = self.local_id().await;
                let req_id = next_id.fetch_add(1, Ordering::SeqCst);
                async move {
                    let msg = KademliaMessage::Store {
                        sender_id,
                        key,
                        value,
                    };
                    let payload = match wire_encode(&msg) {
                        Ok(p) => p,
                        Err(_) => return false,
                    };
                    let mut packet = Vec::with_capacity(8 + payload.len());
                    packet.extend_from_slice(&req_id.to_be_bytes());
                    packet.extend_from_slice(&payload);
                    socket.send_to(&packet, &addr).await.is_ok()
                }
            }));
        }

        // 同时存储到本地
        {
            let mut dht = self.dht.write().await;
            dht.store(key, value);
        }

        let mut success_count = 0usize;
        for h in handles {
            if let Ok(true) = h.await {
                success_count += 1;
            }
        }

        Ok(success_count)
    }
}

impl Drop for DhtTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_transport_bind_and_shutdown() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let transport = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let addr = transport.local_addr().unwrap();
        assert!(addr.port() > 0);

        transport.start_recv_loop();
        assert!(transport.is_running());

        transport.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!transport.is_running());
    }

    #[tokio::test]
    async fn test_transport_ping_pong() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();

        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A -> B: Ping, expect Pong
        let result = ta.ping(&addr_b).await.unwrap();
        assert!(result, "Ping should receive Pong");

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_find_node_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        // Pre-populate B's routing table with some nodes
        {
            let mut b = dht_b.write().await;
            for i in 3..=10u8 {
                let mut id = [0u8; 20];
                id[0] = i;
                b.add_node(DhtNode::new(id, format!("10.0.0.{i}:8080")));
            }
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        let target = [5u8; 20];
        let nodes = ta.find_node_rpc(&addr_b, &target).await.unwrap();
        assert!(
            !nodes.is_empty(),
            "B should return nodes from its routing table"
        );
        assert!(nodes.len() <= K_BUCKET_SIZE);

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_find_value_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        // Store a value at B
        {
            let mut b = dht_b.write().await;
            b.store(b"test_key".to_vec(), b"test_value".to_vec());
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        let (value, _nodes) = ta.find_value_rpc(&addr_b, b"test_key").await.unwrap();
        assert_eq!(value, Some(b"test_value".to_vec()));

        // Key not stored at B
        let (value, _nodes) = ta.find_value_rpc(&addr_b, b"nonexistent").await.unwrap();
        assert_eq!(value, None);

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_store_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b.clone())
            .await
            .unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A sends Store to B
        ta.store_rpc(&addr_b, b"remote_key", b"remote_value")
            .await
            .unwrap();

        // Wait for B to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify B has the value
        let b = dht_b.read().await;
        let (value, _) = b.find_value(b"remote_key");
        assert_eq!(value, Some(b"remote_value".to_vec()));

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_rpc_timeout() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        ta.start_recv_loop();

        // Send RPC to an address where nobody is listening
        // Use a short timeout by sending to a non-routable address
        // For faster test, we use a local port that isn't bound
        let unused_addr = "127.0.0.1:19999";
        let msg = KademliaMessage::Ping {
            sender_id: [1u8; 20],
        };

        // Override timeout for faster test: use tokio::time::timeout directly
        let result =
            tokio::time::timeout(Duration::from_millis(200), ta.send_rpc(unused_addr, &msg)).await;

        // Should timeout
        assert!(
            result.is_err(),
            "RPC to non-listening address should timeout"
        );

        ta.shutdown();
    }

    #[tokio::test]
    async fn test_transport_distributed_store_and_find() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b2 = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let tb = DhtTransport::bind("127.0.0.1:0", dht_b2.clone())
            .await
            .unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        // Add B to A's routing table
        {
            let mut a = dht_a.write().await;
            a.add_node(DhtNode::new([2u8; 20], addr_b));
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a.clone())
            .await
            .unwrap();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // Distributed store: A stores key-value, replicated to B
        let key = b"dist_key".to_vec();
        let value = b"dist_value".to_vec();
        let stored = ta
            .distributed_store(key.clone(), value.clone())
            .await
            .unwrap();
        assert!(stored > 0, "Should store to at least one remote node");

        // Wait for Store RPCs to be processed
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify B has the value
        {
            let b = dht_b2.read().await;
            let (val, _) = b.find_value(&key);
            assert_eq!(val, Some(value.clone()), "B should have the stored value");
        }

        // Distributed find_value: A looks up the key
        let found = ta.distributed_find_value(&key).await.unwrap();
        assert_eq!(found, Some(value));

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_shutdown_stops_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        ta.start_recv_loop();
        ta.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // RPC after shutdown should fail immediately
        let result = ta.ping("127.0.0.1:19999").await;
        assert!(
            matches!(result, Err(TransportError::Shutdown)),
            "RPC after shutdown should return Shutdown error"
        );
    }

    #[tokio::test]
    async fn test_transport_drop_shuts_down() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let running = ta.running.clone();
        ta.start_recv_loop();
        assert!(running.load(Ordering::SeqCst));

        drop(ta);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !running.load(Ordering::SeqCst),
            "Drop should set running to false"
        );
    }

    #[tokio::test]
    async fn test_transport_multiple_pings() {
        let dht_server = Arc::new(RwLock::new(KademliaDht::new([0u8; 20], 100)));
        let server = DhtTransport::bind("127.0.0.1:0", dht_server).await.unwrap();
        let server_addr = server.local_addr().unwrap().to_string();
        server.start_recv_loop();

        // Multiple clients ping the same server
        for i in 1..=5u8 {
            let mut id = [0u8; 20];
            id[0] = i;
            let dht_client = Arc::new(RwLock::new(KademliaDht::new(id, 100)));
            let client = DhtTransport::bind("127.0.0.1:0", dht_client).await.unwrap();
            client.start_recv_loop();

            let result = client.ping(&server_addr).await.unwrap();
            assert!(result, "Client {i} ping should succeed");

            client.shutdown();
        }

        server.shutdown();
    }

    #[tokio::test]
    async fn test_transport_bidirectional_ping() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();

        let addr_a = ta.local_addr().unwrap().to_string();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A pings B
        assert!(ta.ping(&addr_b).await.unwrap());
        // B pings A
        assert!(tb.ping(&addr_a).await.unwrap());

        ta.shutdown();
        tb.shutdown();
    }
}
