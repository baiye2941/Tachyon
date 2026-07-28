const MAX_SAMPLES = 60;
/** 单任务速度采样最小间隔(ms):leading 立即写入,窗内只保留 trailing 最新值 */
const THROTTLE_MS = 500;

/** 可注入时钟,便于假时钟单测;默认 Date.now */
let nowFn: () => number = () => Date.now();

export function setTaskSpeedNow(fn: (() => number) | null) {
  nowFn = fn ?? (() => Date.now());
}

interface TaskSpeedState {
  samples: number[];
  /** 最近一次真正写入采样的时间戳;无写入时为 -Infinity */
  lastCommitAt: number;
  /** 窗内待 trailing 刷入的最新速度 */
  pendingSpeed: number | undefined;
  timer: ReturnType<typeof setTimeout> | null;
}

const historyMap = new Map<string, TaskSpeedState>();

function getOrCreateState(taskId: string): TaskSpeedState {
  let state = historyMap.get(taskId);
  if (!state) {
    state = {
      samples: [],
      lastCommitAt: Number.NEGATIVE_INFINITY,
      pendingSpeed: undefined,
      timer: null,
    };
    historyMap.set(taskId, state);
  }
  return state;
}

function commitSample(state: TaskSpeedState, speed: number, at: number) {
  state.samples.push(speed);
  if (state.samples.length > MAX_SAMPLES) {
    state.samples.shift();
  }
  state.lastCommitAt = at;
  state.pendingSpeed = undefined;
}

function clearTimer(state: TaskSpeedState) {
  if (state.timer !== null) {
    clearTimeout(state.timer);
    state.timer = null;
  }
}

function flushTrailing(taskId: string) {
  const state = historyMap.get(taskId);
  if (!state) return;
  state.timer = null;
  if (state.pendingSpeed === undefined) return;
  const speed = state.pendingSpeed;
  // trailing 落点以触发时刻为准,开启下一 500ms 窗
  commitSample(state, speed, nowFn());
}

/**
 * 记录单任务瞬时速度。
 * 500ms 时间窗:窗首 sample 立即写入;窗内后续 push 只更新 trailing,
 * 在窗末(距上次 commit 满 500ms)写入最新值。
 */
export function pushTaskSpeed(taskId: string, speed: number) {
  if (!taskId) return;

  const state = getOrCreateState(taskId);
  const now = nowFn();
  const elapsed = now - state.lastCommitAt;

  if (elapsed >= THROTTLE_MS) {
    // 已过窗或首次:leading 立即提交,取消未完成的 trailing
    clearTimer(state);
    commitSample(state, speed, now);
    return;
  }

  // 窗内:只保留最新值,并在剩余时间后 trailing 刷入
  state.pendingSpeed = speed;
  if (state.timer === null) {
    const remaining = THROTTLE_MS - elapsed;
    state.timer = setTimeout(() => {
      flushTrailing(taskId);
    }, remaining);
  }
}

export function getTaskHistory(taskId: string): number[] {
  if (!taskId) return [];
  return historyMap.get(taskId)?.samples ?? [];
}

export function clearTaskHistory(taskId: string) {
  if (!taskId) return;
  const state = historyMap.get(taskId);
  if (state) {
    clearTimer(state);
    historyMap.delete(taskId);
  }
}
