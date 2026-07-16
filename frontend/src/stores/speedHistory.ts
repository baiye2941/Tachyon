import { createSignal } from 'solid-js'

const HISTORY_SIZE = 60

// 环形缓冲区，避免每秒数组拷贝
const buffer = new Float64Array(HISTORY_SIZE)
let writeIndex = 0
let count = 0
let sum = 0
let peak = 0

const [activeTasks, setActiveTasks] = createSignal(0)
// 审计 FT-12:version 信号使 StatusBar createMemo 在 pushSpeed 后失效
const [historyVersion, setHistoryVersion] = createSignal(0)

export function pushSpeed(speed: number) {
  if (count === HISTORY_SIZE) {
    const oldVal = buffer[writeIndex]!
    sum -= oldVal
    if (oldVal >= peak) {
      peak = speed
      for (let i = 0; i < HISTORY_SIZE; i++) {
        if (i !== writeIndex && buffer[i]! > peak) {
          peak = buffer[i]!
        }
      }
    }
  } else {
    count++
  }

  buffer[writeIndex] = speed
  sum += speed
  if (speed > peak) peak = speed

  writeIndex = (writeIndex + 1) % HISTORY_SIZE
  setHistoryVersion((v) => v + 1)
}

export function getHistory(): number[] {
  // 订阅 version,保证在 Solid 跟踪上下文中被 pushSpeed 触发重算
  historyVersion()
  const result = new Array<number>(count)
  const start = count === HISTORY_SIZE ? writeIndex : 0
  for (let i = 0; i < count; i++) {
    result[i] = buffer[(start + i) % HISTORY_SIZE]!
  }
  return result
}

/** 测试/调试:历史变更代数 */
export function getHistoryVersion(): number {
  return historyVersion()
}

export function clearHistory() {
  buffer.fill(0)
  writeIndex = 0
  count = 0
  sum = 0
  peak = 0
  setHistoryVersion((v) => v + 1)
}

export function setActiveTasksCount(n: number) {
  setActiveTasks(n)
}

export function getActiveTasks(): number {
  return activeTasks()
}

export function getAverageSpeed(): number {
  historyVersion()
  return count === 0 ? 0 : sum / count
}

export function getPeakSpeed(): number {
  historyVersion()
  return peak
}

export function getCurrentSpeed(): number {
  historyVersion()
  if (count === 0) return 0
  return buffer[(writeIndex - 1 + HISTORY_SIZE) % HISTORY_SIZE]!
}
