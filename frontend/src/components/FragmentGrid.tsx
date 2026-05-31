import { For } from 'solid-js'
import type { DownloadStatus } from '../types'

interface FragmentGridProps {
  total: number
  done: number
  status: DownloadStatus
}

export default function FragmentGrid(props: FragmentGridProps) {
  const blocks = (): Array<'done' | 'active' | 'pending'> => {
    const arr: Array<'done' | 'active' | 'pending'> = []
    for (let i = 0; i < props.total; i++) {
      if (i < props.done) arr.push('done')
      else if (i === props.done && props.status === 'downloading') arr.push('active')
      else arr.push('pending')
    }
    return arr
  }

  const blockClass = (cls: 'done' | 'active' | 'pending') => {
    switch (cls) {
      case 'done': return 'bg-accent'
      case 'active': return 'bg-accent animate-pulse'
      case 'pending': return 'bg-white/10'
    }
  }

  return (
    <div class="grid grid-cols-8 gap-0.5">
      <For each={blocks()}>
        {(cls) => <div class={`w-2 h-2 rounded-sm ${blockClass(cls)}`} />}
      </For>
    </div>
  )
}