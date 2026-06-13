import {
    FileIcon, VideoIcon, AudioIcon, DocumentIcon, ImageIcon,
    ArchiveIcon, GearIcon, AttachmentIcon,
} from '../components/icons'

export const THREAD_COLORS = [
    '#00D4AA', '#00B4D8', '#8B5CF6', '#F59E0B', '#EC4899',
    '#10B981', '#F97316', '#06B6D4', '#A855F7', '#84CC16',
    '#14B8A6', '#D946EF',
]

export function formatSize(bytes: number | null | undefined): string {
    if (bytes === null || bytes === undefined) return '\u672A\u77E5'
    if (bytes === 0) return '0 B'
    if (bytes >= 1024 * 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024 / 1024).toFixed(1)} TB`
    if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`
    if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`
    return `${bytes} B`
}

export function formatSpeed(bytesPerSec: number): string {
    if (bytesPerSec === 0) return '---'
    if (bytesPerSec >= 1024 * 1024 * 1024) return `${(bytesPerSec / 1024 / 1024 / 1024).toFixed(1)} GB/s`
    if (bytesPerSec >= 1024 * 1024) return `${(bytesPerSec / 1024 / 1024).toFixed(1)} MB/s`
    if (bytesPerSec >= 1024) return `${(bytesPerSec / 1024).toFixed(1)} KB/s`
    return `${bytesPerSec} B/s`
}

const FILE_TYPE_MAP: Record<string, { icon: typeof FileIcon; color: string }> = {
    video: { icon: VideoIcon, color: '#F59E0B' },
    audio: { icon: AudioIcon, color: '#8B5CF6' },
    document: { icon: DocumentIcon, color: '#3B82F6' },
    image: { icon: ImageIcon, color: '#10B981' },
    archive: { icon: ArchiveIcon, color: '#F97316' },
    executable: { icon: GearIcon, color: '#6B7280' },
}

const EXT_TYPE_MAP: Record<string, string> = {
    mp4: 'video', mkv: 'video', avi: 'video', mov: 'video', webm: 'video',
    mp3: 'audio', wav: 'audio', flac: 'audio', aac: 'audio', ogg: 'audio',
    pdf: 'document', doc: 'document', docx: 'document', txt: 'document', xls: 'document', xlsx: 'document',
    jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', svg: 'image',
    zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive', gz: 'archive',
    exe: 'executable', msi: 'executable', dmg: 'executable', sh: 'executable',
}

export function getFileType(fileName: string): { icon: typeof FileIcon; color: string } {
    const ext = fileName.split('.').pop()?.toLowerCase() || ''
    const type = EXT_TYPE_MAP[ext]
    const entry = type ? FILE_TYPE_MAP[type] : undefined
    return entry ? { icon: entry.icon, color: entry.color } : { icon: AttachmentIcon, color: '#6B7280' }
}

export function getFileTypeColor(type: string): string {
    return FILE_TYPE_MAP[type]?.color ?? '#6B7280'
}

export function getStatusColor(status: string): string {
    switch (status) {
        case 'downloading': return '#00D4AA'
        case 'pending': return '#6B7280'
        case 'paused': return '#F59E0B'
        case 'completed': return '#00D4AA'
        case 'failed': return '#EF4444'
        case 'connecting': return '#00B4D8'
        case 'verifying': return '#00B4D8'
        case 'resuming': return '#00B4D8'
        default: return '#6B7280'
    }
}

export function statusColor(status: string): string {
    switch (status) {
        case 'downloading': return 'bg-[#00D4AA]'
        case 'pending': return 'bg-[#6B7280]'
        case 'paused': return 'bg-[#F59E0B]'
        case 'completed': return 'bg-[#00D4AA]'
        case 'failed': return 'bg-[#EF4444]'
        case 'connecting':
        case 'verifying':
        case 'resuming':
            return 'bg-[#00B4D8]'
        default: return 'bg-[#6B7280]'
    }
}

export function getStatusLabel(status: string): string {
    switch (status) {
        case 'downloading': return '\u4E0B\u8F7D\u4E2D'
        case 'pending': return '\u7B49\u5F85\u4E2D'
        case 'paused': return '\u5DF2\u6682\u505C'
        case 'completed': return '\u5DF2\u5B8C\u6210'
        case 'failed': return '\u51FA\u9519'
        case 'connecting': return '\u8FDE\u63A5\u4E2D'
        case 'verifying': return '\u6821\u9A8C\u4E2D'
        case 'resuming': return '\u6062\u590D\u4E2D'
        default: return status
    }
}

export function formatETA(speed: number, remaining: number): string {
    if (speed <= 0 || remaining <= 0) return '---'
    const seconds = Math.ceil(remaining / speed)
    if (seconds < 60) return `${seconds} \u79D2`
    if (seconds < 3600) {
        const m = Math.floor(seconds / 60)
        const s = seconds % 60
        return `${m} \u5206 ${s} \u79D2`
    }
    const h = Math.floor(seconds / 3600)
    const m = Math.floor((seconds % 3600) / 60)
    return `${h} \u5C0F\u65F6 ${m} \u5206`
}

export function formatDate(iso: string): string {
    const d = new Date(iso)
    const pad = (n: number) => n.toString().padStart(2, '0')
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}
