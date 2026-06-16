/**
 * 任务列表列定义(Iteration 07,DI-3)。
 *
 * 单一数据来源:TaskList 表头与 TaskItem 数据行共用本配置,
 * 消除原「表头 120px/100px/80px 与行内手写同样数值」的两处对齐。
 * 支持列排序(sortable),供 taskSort store 驱动。
 */

export type SortKey = 'name' | 'progress' | 'speed' | 'status'
export type SortDir = 'asc' | 'desc'

export interface ColumnDef {
  key: SortKey
  label: string
  /** CSS 宽度。'flex-1' 表示弹性填充(文件名列) */
  width: string
  align: 'left' | 'right'
  /** 是否支持点击排序 */
  sortable: boolean
}

/** 列定义(顺序 = 渲染顺序) */
export const COLUMNS: ColumnDef[] = [
  { key: 'name', label: '文件名', width: 'flex-1', align: 'left', sortable: false },
  { key: 'progress', label: '进度', width: '120px', align: 'right', sortable: true },
  { key: 'speed', label: '速度', width: '100px', align: 'right', sortable: true },
  { key: 'status', label: '状态', width: '80px', align: 'right', sortable: true },
]

/** 可排序列 */
export const SORTABLE_KEYS: SortKey[] = COLUMNS.filter((c) => c.sortable).map((c) => c.key)

/**
 * 列宽常量(TaskItem 行内渲染引用,保证表头/行单一来源)。
 * 文件名列用 flex-1,此处不列。
 */
export const COLUMN_WIDTH = {
  progress: '120px',
  speed: '100px',
  status: '80px',
} as const
