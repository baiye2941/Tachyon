import { onCleanup } from 'solid-js'
import { onProgressUpdate } from '../api/events'
import { updateProgress, refreshTaskList } from '../stores/downloads'
import { addToast } from '../stores/toast'
import { tr } from '../i18n'
import { errorMessage } from '../utils/appError'

/** 审计 FT-05:进度监听失败重连上限与校准间隔 */
const MAX_RECONNECT_ATTEMPTS = 8
const BASE_BACKOFF_MS = 500
const MAX_BACKOFF_MS = 15_000
const SNAPSHOT_CALIBRATE_MS = 15_000

/**
 * 订阅 progress-update;注册失败指数退避重连,并周期 refreshTaskList 校准。
 */
export function useProgressListener() {
  let disposed = false
  let activeUnlisten: (() => void) | undefined
  let reconnectTimer: number | undefined
  let calibrateTimer: number | undefined
  let attempts = 0

  const clearTimers = () => {
    if (reconnectTimer !== undefined) {
      window.clearTimeout(reconnectTimer)
      reconnectTimer = undefined
    }
    if (calibrateTimer !== undefined) {
      window.clearInterval(calibrateTimer)
      calibrateTimer = undefined
    }
  }

  const startCalibration = () => {
    if (calibrateTimer !== undefined) return
    calibrateTimer = window.setInterval(() => {
      if (disposed) return
      void refreshTaskList()
    }, SNAPSHOT_CALIBRATE_MS)
  }

  const scheduleReconnect = () => {
    if (disposed) return
    if (attempts >= MAX_RECONNECT_ATTEMPTS) {
      addToast(
        tr('toast.progressListenFailed', { error: 'max reconnects' }),
        'error',
      )
      // 仍保持低频校准,避免进度永久冻结
      startCalibration()
      return
    }
    const delay = Math.min(MAX_BACKOFF_MS, BASE_BACKOFF_MS * 2 ** attempts)
    attempts += 1
    reconnectTimer = window.setTimeout(() => {
      void connect()
    }, delay)
  }

  const connect = async () => {
    if (disposed) return
    try {
      const unlisten = await onProgressUpdate((payload) => {
        if (!payload || typeof payload !== 'object') return
        updateProgress(payload)
      })
      if (disposed) {
        unlisten()
        return
      }
      activeUnlisten = unlisten
      attempts = 0
      startCalibration()
    } catch (e) {
      if (attempts === 0) {
        addToast(
          tr('toast.progressListenFailed', { error: errorMessage(e) }),
          'error',
        )
      }
      scheduleReconnect()
    }
  }

  void connect()

  onCleanup(() => {
    disposed = true
    clearTimers()
    activeUnlisten?.()
    activeUnlisten = undefined
  })
}
