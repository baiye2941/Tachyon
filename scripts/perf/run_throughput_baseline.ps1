#Requires -Version 5.1
<#
.SYNOPSIS
  可重复吞吐基线场景矩阵编排(Windows)

.DESCRIPTION
  跑本地 ThrottledServer 场景矩阵,可选 aria2 对标,输出 JSON 到 target/perf-baseline/。
  外部 CDN/HF 双源请传 -PrimaryUrl / -MirrorUrl。

.EXAMPLE
  .\scripts\perf\run_throughput_baseline.ps1 -Quick
  .\scripts\perf\run_throughput_baseline.ps1 -Size 512MiB -CompareAria2
  .\scripts\perf\run_throughput_baseline.ps1 -PrimaryUrl https://... -MirrorUrl https://mirror/...
#>
param(
    [switch]$Quick,
    [string]$Size = "64MiB",
    [int]$Runs = 3,
    [int]$Concurrency = 16,
    [switch]$CompareAria2,
    [string]$PrimaryUrl = "",
    [string]$MirrorUrl = "",
    [string]$OutDir = "target/perf-baseline"
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Invoke-Baseline {
    param(
        [string]$Name,
        [string[]]$ExtraArgs
    )
    $out = Join-Path $OutDir "$Name.json"
    Write-Host "=== scenario: $Name ===" -ForegroundColor Cyan
    $args = @("bench", "--bench", "throughput_baseline", "--") + $ExtraArgs + @("--out", $out)
    if ($CompareAria2) { $args += "--compare-aria2" }
    & cargo @args
    if ($LASTEXITCODE -ne 0) {
        throw "baseline failed: $Name (exit $LASTEXITCODE)"
    }
}

$env:RUST_LOG = if ($env:RUST_LOG) { $env:RUST_LOG } else { "warn" }

if ($PrimaryUrl) {
    $extra = @("--url", $PrimaryUrl, "--runs", "$Runs", "--concurrency", "$Concurrency")
    if ($MirrorUrl) { $extra += @("--mirror", $MirrorUrl) }
    Invoke-Baseline -Name "external_primary" -ExtraArgs $extra
    Write-Host "done. results under $OutDir"
    exit 0
}

# 场景矩阵(本地 server)
$scenarios = @(
    @{ Name = "loopback_unthrottled"; Args = @("--size", $Size, "--rtt-ms", "0", "--bps", "0", "--runs", "$Runs", "--concurrency", "$Concurrency") }
)

if (-not $Quick) {
    $scenarios += @(
        @{ Name = "rtt50"; Args = @("--size", $Size, "--rtt-ms", "50", "--bps", "0", "--runs", "$Runs", "--concurrency", "$Concurrency") }
        @{ Name = "rtt100"; Args = @("--size", $Size, "--rtt-ms", "100", "--bps", "0", "--runs", "$Runs", "--concurrency", "$Concurrency") }
        @{ Name = "rtt200"; Args = @("--size", $Size, "--rtt-ms", "200", "--bps", "0", "--runs", "$Runs", "--concurrency", "$Concurrency") }
        @{ Name = "cap_100Mbps"; Args = @("--size", $Size, "--rtt-ms", "0", "--bps", "12.5M", "--runs", "$Runs", "--concurrency", "$Concurrency") }
        @{ Name = "cap_1Gbps_rtt50"; Args = @("--size", $Size, "--rtt-ms", "50", "--bps", "125M", "--runs", "$Runs", "--concurrency", "$Concurrency") }
    )
}

foreach ($s in $scenarios) {
    Invoke-Baseline -Name $s.Name -ExtraArgs $s.Args
}

Write-Host ""
Write-Host "全部完成. JSON: $OutDir" -ForegroundColor Green
Write-Host "指标字段: goodput_bps / aligned_write_* / rebalance_count / peak_active_requests"
Write-Host "CPU%/磁盘队列: 请用资源监视器/typeperf 外挂采样(本 harness 不伪造)"
Write-Host "丢包 0/1/2%: 需管理员 netem/clumsy/网关模拟;脚本本身不做内核级 netem"
Write-Host "文档: docs/sdd/throughput-baseline.md"
