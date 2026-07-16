# Evidence — residual: raw cache key + full-stream short write

## 1. Remove raw URL dual-write from HandleCache

### Before
- insert: binding key + (if preferred=None) raw magnet URL
- lookup/get: bind_key 失败回退 raw url（上下文匹配时）

### After
- insert **only** `cache_binding_key(dir|factory|preferred|url)`
- `lookup_compatible` **only** bind_key
- production get paths only bind_key
- `from_handle` test seam only bind_key
- tests assert via `lookup_compatible`

### Tests
```
cargo nextest run -p tachyon-protocol -- magnet::tests
# 59/59 pass
```

## 2. full-stream short write

### Before
`execute_full_download` 对每个 chunk 单次 `storage.write_at`，短写时 `pos += written` 丢数据。

### After
对齐分片路径：`DownloadTask::write_all_at` 循环写完整个 chunk。

### Tests
```
cargo nextest run -p tachyon-engine --features test-harness -- \
  write_all_at_retries_short_write full_download_survives_storage_short_write
# pass
```

## 3. DNS cache + async resolve (HTTP-14)

### Before
- `PublicDnsResolver.cache: DashMap`（非 Arc）
- `resolve` 内 `let cache = self.cache.clone()` → DashMap **内容拷贝**，insert 不回写
- async future 内同步 `to_socket_addrs` 阻塞 worker

### After
- `cache: Arc<DashMap<...>>`，resolve 用 `Arc::clone`
- 系统解析走 `tokio::task::spawn_blocking`
- 测试：`test_public_dns_resolver_cache_shared_across_resolves`、`test_public_dns_resolver_clone_shares_cache`

## 4. Permanent 4xx not retryable (HTTP-08)

### Before
- `classify_http_error` 非 401/403/429/503 → `DownloadError::Protocol`
- `is_retryable` 对 `Protocol` 默认 **true** → 404/410/416 空转重试

### After
- 其余状态码 → `DownloadError::Http { status, reason }`
- `is_retryable` 已有 Http 分支：仅 408/429/5xx 可重试
- 带上下文版本同构（URL 进 reason）
- 测试：permanent 4xx / 404 / 408 / 500 / head 404 evaluate

```
http::tests 107/107 pass
```

## 5. HTTP-04 probe Content-Range

`probe_via_get_range` 206 分支：`validate_content_range(headers, 0, 0)?`
测试：`test_probe_get_range_rejects_mismatched_content_range` / `..._missing_content_range`

## 6. HTTP-01 write_buf retry clear

`spawn_fragment_task` retry loop 内、`download_single_fragment` 前：`write_buf.as_mut().clear()`
测试：`test_fragment_retry_clears_write_buf_between_attempts`

## 7. HTTP-11 socks feature

根 `Cargo.toml` reqwest features 增加 `socks`
测试：`test_socks5_proxy_builds_client`

## 8. HTTP-10 UA/headers

- `with_timeouts_and_headers` / `with_connection_config_and_headers`
- `build_default_headers` 过滤 reserved + CRLF
- engine 三处 HttpClient 构造透传 `config.user_agent` / `config.headers`
测试：`test_custom_user_agent_sent` / `test_custom_headers_sent_and_reserved_stripped` / `test_build_default_headers_skips_reserved`
