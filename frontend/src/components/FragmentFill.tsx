import { Motion } from "@motionone/solid";
import { Show, type JSX } from "solid-js";

interface FragmentFillProps {
  /** 分片已下载比例 [0, 1] */
  progress: number;
  /** 是否减少动画(reduced-motion 降级为静态 transform) */
  reducedMotion: boolean;
}

/**
 * 分片半填充充能条:在 chunk-cell 内部从左到右填充,弹簧物理驱动。
 *
 * 性能:仅动 transform: scaleX(GPU 合成,零 reflow)。reduced-motion 降级为
 * 静态 transform(无 spring,无 rAF),并带 chunk-cell-fill--static 修饰类
 * 关掉 will-change(静态分支无合成需求)。父级 chunk-cell 设 overflow: hidden +
 * position: relative,本组件 absolute inset-0。
 *
 * Motion 用法对齐 DetailPanel/CommandPalette:animate 传独立变换值 scaleX
 * (数值),而非 transform 字符串——spring 发生器只能对数值生成关键帧,
 * 字符串会丢失弹簧物理。Motion 组件的 animate prop 是响应式的(内部
 * createEffect 追踪),progress 变化时 spring 从当前值平滑 retarget 到新值。
 *
 * spring 参数:stiffness 300 / damping 30 / mass 0.8(与 DetailPanel 滑入一致)。
 */
export default function FragmentFill(props: FragmentFillProps): JSX.Element {
  const pct = () => Math.max(0, Math.min(1, props.progress));

  // Show 分支而非 early return:reducedMotion 是响应式 prop(OS 设置可热切换),
  // early return 会冻结在首次渲染的分支(eslint solid/components-return-once)。
  return (
    <Show
      when={props.reducedMotion}
      fallback={
        <Motion.div
          class="chunk-cell-fill"
          initial={{ scaleX: 0 }}
          animate={{ scaleX: pct() }}
          transition={{
            type: "spring",
            stiffness: 300,
            damping: 30,
            mass: 0.8,
          }}
          aria-hidden="true"
        />
      }
    >
      <div
        class="chunk-cell-fill chunk-cell-fill--static"
        style={{ transform: `scaleX(${pct()})` }}
        aria-hidden="true"
      />
    </Show>
  );
}
