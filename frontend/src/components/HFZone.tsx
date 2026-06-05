import { For, createSignal, createResource, Show, onMount, onCleanup } from 'solid-js'
import { formatSize } from '../utils/format'

interface LFSFileNode {
  name: string
  path: string
  size: number
  children: LFSFileNode[]
}

interface ConnectionSlot {
  id: string
  state: 'handshake' | 'downloading' | 'writing' | 'verifying' | 'idle'
  speed: number
}

const SLOT_STATE_COLORS: Record<ConnectionSlot['state'], string> = {
  handshake: 'bg-warning',
  downloading: 'bg-accent',
  writing: 'bg-accent',
  verifying: 'bg-success',
  idle: 'bg-text-tertiary',
}

const SLOT_STATE_LABELS: Record<ConnectionSlot['state'], string> = {
  handshake: '握手',
  downloading: '下载',
  writing: '写入',
  verifying: '校验',
  idle: '空闲',
}

const MOCK_TREE: LFSFileNode[] = [
  {
    name: 'models', path: 'models', size: 0, children: [
      { name: 'pytorch_model.bin', path: 'models/pytorch_model.bin', size: 4_294_967_296, children: [] },
      { name: 'config.json', path: 'models/config.json', size: 1_024, children: [] },
    ]
  },
  {
    name: 'data', path: 'data', size: 0, children: [
      { name: 'train.parquet', path: 'data/train.parquet', size: 2_147_483_648, children: [] },
    ]
  },
  { name: 'README.md', path: 'README.md', size: 4_096, children: [] },
]

const MOCK_SLOTS: ConnectionSlot[] = [
  { id: 's1', state: 'downloading', speed: 12_400_000 },
  { id: 's2', state: 'downloading', speed: 10_800_000 },
  { id: 's3', state: 'writing', speed: 0 },
  { id: 's4', state: 'verifying', speed: 0 },
  { id: 's5', state: 'handshake', speed: 0 },
  { id: 's6', state: 'idle', speed: 0 },
]

function TreeNode(props: { node: LFSFileNode; depth: number }) {
  const [expanded, setExpanded] = createSignal(false)
  const hasChildren = () => props.node.children.length > 0

  return (
    <div>
      <button
        class="w-full flex items-center gap-2 px-2 py-1.5 text-left text-[13px] hover:bg-white/[0.04] transition-colors duration-150 rounded"
        style={{ 'padding-left': `${props.depth * 16 + 8}px` }}
        onClick={() => setExpanded(!expanded())}
      >
        <Show when={hasChildren()}>
          <svg class={`w-3 h-3 shrink-0 text-text-tertiary transition-transform duration-200 ${expanded() ? 'rotate-90' : ''}`} viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5">
            <path d="M4 2l4 4-4 4" />
          </svg>
        </Show>
        <Show when={!hasChildren()}>
          <span class="w-3 shrink-0" />
        </Show>
        <span class={hasChildren() ? 'text-text-primary font-medium' : 'text-text-secondary'}>{props.node.name}</span>
        <Show when={props.node.size > 0}>
          <span class="ml-auto text-[11px] font-mono text-text-tertiary">{formatSize(props.node.size)}</span>
        </Show>
      </button>
      <Show when={hasChildren() && expanded()}>
        <For each={props.node.children}>
          {(child) => <TreeNode node={child} depth={props.depth + 1} />}
        </For>
      </Show>
    </div>
  )
}

function SlotMap(props: { slots: ConnectionSlot[] }) {
  return (
    <div class="grid grid-cols-3 gap-2">
      <For each={props.slots}>
        {(slot) => (
          <div
            class="glass-panel rounded p-2 flex items-center gap-2 transition-colors duration-200"
            title={`${slot.id}: ${slot.state}`}
          >
            <div class={`w-[3px] h-6 rounded-full ${SLOT_STATE_COLORS[slot.state]}`} />
            <div class="flex flex-col">
              <span class="text-[10px] font-mono text-text-secondary">{slot.id}</span>
              <span class="text-[10px] font-mono text-text-primary">{SLOT_STATE_LABELS[slot.state]}</span>
            </div>
            <Show when={slot.speed > 0}>
              <span class="ml-auto text-[10px] font-mono text-accent">{formatSize(slot.speed)}/s</span>
            </Show>
          </div>
        )}
      </For>
    </div>
  )
}

function ThroughputWave() {
  const [points, setPoints] = createSignal<string>('')
  const [offset, setOffset] = createSignal(0)

  let frameId: number
  let visible = true
  let containerRef: HTMLDivElement | undefined
  let observer: IntersectionObserver | undefined

  onMount(() => {
    if (containerRef) {
      observer = new IntersectionObserver(([entry]) => {
        visible = entry.isIntersecting
      }, { threshold: 0 })
      observer.observe(containerRef)
    }
  })

  onCleanup(() => {
    cancelAnimationFrame(frameId)
    observer?.disconnect()
  })

  const animate = () => {
    if (!visible) {
      frameId = requestAnimationFrame(animate)
      return
    }
    setOffset(prev => prev + 0.8)
    const w = 280
    const h = 60
    const pts: string[] = []
    for (let x = 0; x <= w; x += 4) {
      const t = x / w * Math.PI * 4 - offset()
      const y = h / 2 + Math.sin(t) * 14 + Math.sin(t * 2.7) * 6
      pts.push(`${x},${y.toFixed(1)}`)
    }
    setPoints(pts.join(' '))
    frameId = requestAnimationFrame(animate)
  }

  onMount(() => {
    animate()
  })

  const gradientId = 'throughput-grad'

  return (
    <div ref={containerRef}>
      <svg viewBox="0 0 280 60" class="w-full h-[60px]" preserveAspectRatio="none" aria-label="吞吐量波形">
        <defs>
          <linearGradient id="throughput-area" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stop-color="var(--color-accent)" />
            <stop offset="100%" stop-color="transparent" />
          </linearGradient>
          <linearGradient id={gradientId} x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stop-color="var(--color-accent)" />
            <stop offset="100%" stop-color="var(--color-accent)" />
          </linearGradient>
        </defs>
        <path
          d={`M${points()} L280,60 L0,60 Z`}
          fill="url(#throughput-area)"
          opacity="0.08"
        />
        <path
          d={`M${points()}`}
          fill="none"
          stroke={`url(#${gradientId})`}
          stroke-width="1.5"
          stroke-dasharray="4 2"
          stroke-linecap="round"
        />
      </svg>
    </div>
  )
}

export default function HFZone() {
  const [repoId, setRepoId] = createSignal('')
  const [expanded, setExpanded] = createSignal(false)

  const fetchTree = async (_expanded: boolean): Promise<LFSFileNode[]> => {
    return MOCK_TREE
  }

  const [treeData] = createResource(expanded, fetchTree)

  const handleSubmit = (e: Event) => {
    e.preventDefault()
    setExpanded(true)
  }

  return (
    <div class="space-y-6">
      <div>
        <h2 class="text-[18px] font-semibold text-text-primary tracking-tight">Hugging Face LFS</h2>
        <p class="mt-1 text-[13px] text-text-secondary">输入 Repo ID 浏览 LFS 文件树</p>
      </div>

      <form onSubmit={handleSubmit} class="flex gap-2">
        <input
          type="text"
          value={repoId()}
          onInput={(e) => setRepoId(e.currentTarget.value)}
          placeholder="bert-base-uncased"
          class="flex-1 px-3 py-2 bg-surface border border-white/[0.06] rounded text-[13px] font-mono text-text-primary placeholder:text-text-tertiary outline-none focus:border-accent transition-colors duration-150"
        />
        <button
          type="submit"
          class="px-4 py-2 bg-accent text-canvas text-[12px] font-semibold rounded hover:opacity-85 active:scale-[0.98] transition-all duration-100"
        >
          浏览
        </button>
      </form>

      <Show when={expanded()}>
        <div class="border border-white/[0.06] rounded-lg overflow-hidden glass-panel">
          <div class="px-3 py-2 border-b border-white/[0.06] text-[11px] font-semibold text-text-tertiary uppercase tracking-wider">
            文件树
          </div>
          <div class="p-1">
            <Show when={!treeData.loading} fallback={<div class="px-3 py-4 text-text-tertiary text-[13px]">加载中...</div>}>
              <For each={treeData() ?? []}>
                {(node) => <TreeNode node={node} depth={0} />}
              </For>
            </Show>
          </div>
        </div>
      </Show>

      <Show when={expanded()}>
        <div>
          <h3 class="text-[15px] font-medium text-text-primary mb-3">连接槽位</h3>
          <SlotMap slots={MOCK_SLOTS} />
        </div>

        <div>
          <h3 class="text-[15px] font-medium text-text-primary mb-3">吞吐量</h3>
          <div class="glass-panel rounded-lg p-3">
            <ThroughputWave />
          </div>
        </div>
      </Show>
    </div>
  )
}
