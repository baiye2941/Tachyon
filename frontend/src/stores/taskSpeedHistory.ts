const MAX_SAMPLES = 60;

const historyMap = new Map<string, number[]>();

export function pushTaskSpeed(taskId: string, speed: number) {
  if (!taskId) return;

  let samples = historyMap.get(taskId);
  if (!samples) {
    samples = [];
    historyMap.set(taskId, samples);
  }

  samples.push(speed);
  if (samples.length > MAX_SAMPLES) {
    samples.shift();
  }
}

export function getTaskHistory(taskId: string): number[] {
  if (!taskId) return [];
  return historyMap.get(taskId) ?? [];
}

export function clearTaskHistory(taskId: string) {
  if (!taskId) return;
  historyMap.delete(taskId);
}
