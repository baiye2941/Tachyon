import { For, Show, createSignal, onMount } from 'solid-js'
import { api } from '../api/invoke'
import { $config, $configLoading } from '../stores/settings'
import { addToast } from '../stores/toast'
import Toggle from './Toggle'
import type { AppConfig } from '../types'
import { inputBase, btnPrimary } from '../utils/styles'

const ALGORITHMS = ['blake3', 'sha256'] as const
const MIN_SIZE_OPTIONS = [
  { value: 0, label: '无限制' },
  { value: 1048576, label: '1 MB' },
  { value: 5242880, label: '5 MB' },
  { value: 10485760, label: '10 MB' },
  { value: 52428800, label: '50 MB' },
] as const

export default function SettingsPanel() {
  const loading = () => $configLoading.get()

  const [dir, setDir] = createSignal('')
  const [maxTasks, setMaxTasks] = createSignal(3)
  const [maxFragments, setMaxFragments] = createSignal(8)
  const [maxConnections, setMaxConnections] = createSignal(4)
  const [quicEnabled, setQuicEnabled] = createSignal(false)
  const [http2Enabled, setHttp2Enabled] = createSignal(true)
  const [verifyEnabled, setVerifyEnabled] = createSignal(true)
  const [algorithm, setAlgorithm] = createSignal('blake3')
  const [minSize, setMinSize] = createSignal(0)
  const [saved, setSaved] = createSignal(false)

  onMount(async () => {
    try {
      const cfg = await api.getConfig()
      $config.set(cfg)
      setDir(cfg.download.downloadDir)
      setMaxTasks(cfg.maxConcurrentTasks)
      setMaxFragments(cfg.download.maxConcurrentFragments)
      setMaxConnections(cfg.connection.maxConnectionsPerHost)
      setQuicEnabled(cfg.connection.enableQuic)
      setHttp2Enabled(cfg.connection.enableHttp2)
      setVerifyEnabled(cfg.download.verifyChecksum)
      // 如果后端返回 minFragmentSize，映射到 minSize 选择器
      setMinSize(cfg.scheduler.minFragmentSize)
    } catch (e) {
      addToast('加载配置失败: ' + String(e), 'error')
    } finally {
      $configLoading.set(false)
    }
  })

  async function handleSave() {
    const base = $config.get()
    if (!base) return

    const cfg: AppConfig = {
      maxConcurrentTasks: maxTasks(),
      download: {
        downloadDir: dir(),
        maxConcurrentFragments: maxFragments(),
        verifyChecksum: verifyEnabled(),
        maxRetries: base.download.maxRetries,
        requestTimeoutSecs: base.download.requestTimeoutSecs,
        userAgent: base.download.userAgent,
      },
      connection: {
        maxConnectionsPerHost: maxConnections(),
        enableQuic: quicEnabled(),
        enableHttp2: http2Enabled(),
        maxGlobalConnections: base.connection.maxGlobalConnections,
        keepAliveTimeoutSecs: base.connection.keepAliveTimeoutSecs,
        connectTimeoutSecs: base.connection.connectTimeoutSecs,
      },
      scheduler: {
        minFragmentSize: base.scheduler.minFragmentSize,
        maxFragmentSize: base.scheduler.maxFragmentSize,
        samplingIntervalSecs: base.scheduler.samplingIntervalSecs,
        ewmaAlpha: base.scheduler.ewmaAlpha,
      },
    }
    try {
      await api.updateConfig(cfg)
      $config.set(cfg)
      setSaved(true)
      setTimeout(() => setSaved(false), 1500)
      addToast('配置已保存', 'success')
    } catch (e) {
      addToast('保存配置失败: ' + String(e), 'error')
    }
  }

  const fieldClass = `${inputBase} w-20 text-center`

  return (
    <Show
      when={!loading()}
      fallback={
        <div class="flex flex-col items-center justify-center py-10">
          <div class="text-text-secondary text-[13px]">加载配置中...</div>
        </div>
      }
    >
      <div>
        {/* 下载 */}
        <div class="glass-panel rounded-lg p-4 mb-4">
          <div class="text-[11px] font-semibold text-text-tertiary uppercase tracking-wider mb-3">下载</div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">下载目录</span>
            <input
              class={inputBase}
              type="text"
              value={dir()}
              onInput={(e) => setDir(e.currentTarget.value)}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">最大并发任务</span>
            <input
              class={fieldClass}
              type="number"
              min="1"
              max="20"
              value={maxTasks()}
              onInput={(e) => setMaxTasks(Number(e.currentTarget.value) || 1)}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">最大分片数</span>
            <input
              class={fieldClass}
              type="number"
              min="1"
              max="128"
              value={maxFragments()}
              onInput={(e) => setMaxFragments(Number(e.currentTarget.value) || 1)}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">每主机最大连接数</span>
            <input
              class={fieldClass}
              type="number"
              min="1"
              max="32"
              value={maxConnections()}
              onInput={(e) => setMaxConnections(Number(e.currentTarget.value) || 1)}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5">
            <span class="text-[13px] text-text-secondary">最小文件大小</span>
            <select
              class={`${inputBase} w-28`}
              value={minSize()}
              onChange={(e) => setMinSize(Number(e.currentTarget.value))}
            >
              <For each={MIN_SIZE_OPTIONS}>
                {(opt) => <option value={opt.value}>{opt.label}</option>}
              </For>
            </select>
          </div>
        </div>

        {/* 协议 */}
        <div class="glass-panel rounded-lg p-4 mb-4">
          <div class="text-[11px] font-semibold text-text-tertiary uppercase tracking-wider mb-3">协议</div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">QUIC 多路径</span>
            <Toggle
              checked={quicEnabled()}
              ariaLabel="QUIC 开关"
              onChange={setQuicEnabled}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5">
            <span class="text-[13px] text-text-secondary">HTTP/2</span>
            <Toggle
              checked={http2Enabled()}
              ariaLabel="HTTP/2 开关"
              onChange={setHttp2Enabled}
            />
          </div>
        </div>

        {/* 校验 */}
        <div class="glass-panel rounded-lg p-4 mb-4">
          <div class="text-[11px] font-semibold text-text-tertiary uppercase tracking-wider mb-3">校验</div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5 border-b border-white/5">
            <span class="text-[13px] text-text-secondary">启用校验</span>
            <Toggle
              checked={verifyEnabled()}
              ariaLabel="校验开关"
              onChange={setVerifyEnabled}
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-4 items-center py-2.5">
            <span class="text-[13px] text-text-secondary">校验算法</span>
            <select
              class={`${inputBase} w-28`}
              value={algorithm()}
              onChange={(e) => setAlgorithm(e.currentTarget.value)}
            >
              <For each={ALGORITHMS}>
                {(alg) => <option value={alg}>{alg === 'blake3' ? 'Blake3' : 'SHA-256'}</option>}
              </For>
            </select>
          </div>
        </div>

        <div class="flex items-center gap-3 mt-2">
          <button
            class={btnPrimary}
            onClick={handleSave}
          >
            保存配置
          </button>
          <Show when={saved()}>
            <span class="text-success text-[12px] font-semibold">
              已保存
            </span>
          </Show>
        </div>
      </div>
    </Show>
  )
}
