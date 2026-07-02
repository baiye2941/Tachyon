import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Index,
  onCleanup,
  Show,
  untrack,
} from "solid-js";
import { useReducedMotion } from "../hooks/useReducedMotion";

const DIGITS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

interface AnimatedDigitProps {
  target: number;
  duration: number;
  reducedMotion: boolean;
}

/**
 * 单个数字滚轮。
 *
 * 0-9 数字垂直排列,通过 CSS transform + transition 滚动到目标数字。
 * 使用 GPU 加速的 compositor 动画,避免 requestAnimationFrame 在主线程逐帧 setState 导致卡顿。
 */
function AnimatedDigit(props: AnimatedDigitProps) {
  const [current, setCurrent] = createSignal(untrack(() => props.target));

  createEffect(() => {
    setCurrent(props.target);
  });

  return (
    <span class="animated-digit" aria-hidden="true">
      <span
        class="animated-digit-strip"
        style={{
          transform: `translateY(-${current() * 10}%)`,
          "transition-duration": props.reducedMotion
            ? "0ms"
            : `${props.duration}ms`,
        }}
      >
        <For each={DIGITS}>
          {(d) => <span class="animated-digit-number">{d}</span>}
        </For>
      </span>
    </span>
  );
}

interface AnimatedNumberProps {
  value: string | number;
  class?: string;
  duration?: number;
  reducedMotion?: boolean;
  /** 数值刷新节流间隔(ms),防止高频更新导致动画重叠。默认 80ms。 */
  throttleMs?: number;
}

/**
 * 翻页数字组件。
 *
 * 将字符串中的每个阿拉伯数字渲染为独立滚轮,非数字字符保持静态。
 * 适用于下载百分比、速度、大小等高频变化的读数。
 *
 * 性能要点:
 * - 动画走 CSS transition(GPU 合成层),不在 JS 主线程逐帧计算。
 * - 对外部 props.value 做节流,避免下载进度高频刷新时动画堆叠。
 * - 使用 <Index /> 保持每个字符位的组件实例稳定,只更新目标数字。
 *
 * 视觉参考:机械里程表 / Codex 行数变化特效。
 */
export default function AnimatedNumber(props: AnimatedNumberProps) {
  const systemReduced = useReducedMotion();
  const reduced = () => props.reducedMotion ?? systemReduced();

  const [displayValue, setDisplayValue] = createSignal(
    untrack(() => String(props.value)),
  );

  let lastUpdate = 0;
  let pendingTimer: ReturnType<typeof setTimeout> | null = null;

  const clearPending = () => {
    if (pendingTimer) {
      clearTimeout(pendingTimer);
      pendingTimer = null;
    }
  };

  createEffect(() => {
    const newValue = String(props.value);
    const throttleMs = props.throttleMs ?? 80;
    const now = Date.now();

    if (now - lastUpdate >= throttleMs) {
      clearPending();
      lastUpdate = now;
      setDisplayValue(newValue);
      return;
    }

    if (!pendingTimer) {
      pendingTimer = setTimeout(
        () => {
          pendingTimer = null;
          lastUpdate = Date.now();
          setDisplayValue(String(props.value));
        },
        throttleMs - (now - lastUpdate),
      );
    }
  });

  onCleanup(() => clearPending());

  const chars = createMemo(() => displayValue().split(""));

  return (
    <span
      class={`animated-number ${props.class ?? ""}`}
      aria-live="off"
      aria-atomic="true"
    >
      <Index each={chars()}>
        {(char) => {
          const isDigit = () => {
            const c = char();
            return c >= "0" && c <= "9";
          };
          const digit = () => parseInt(char(), 10);
          return (
            <Show
              when={isDigit()}
              fallback={<span class="animated-number-char">{char()}</span>}
            >
              <AnimatedDigit
                target={digit()}
                duration={props.duration ?? 520}
                reducedMotion={reduced()}
              />
            </Show>
          );
        }}
      </Index>
    </span>
  );
}
