import { For, Show, createMemo } from "solid-js";

interface ProgressCelebrationProps {
  reducedMotion?: boolean;
  particleCount?: number;
}

export default function ProgressCelebration(props: ProgressCelebrationProps) {
  // eslint-disable-next-line solid/reactivity -- particleCount is an initial seed, not expected to change
  const particleCount = props.particleCount ?? 8;
  const particles = createMemo(() =>
    Array.from({ length: particleCount }, (_, i) => ({
      id: i,
      angle: (i / particleCount) * 360 + (Math.random() - 0.5) * 20,
      distance: 18 + Math.random() * 12,
      delay: Math.random() * 80,
      scale: 0.5 + Math.random() * 0.5,
    })),
  );

  return (
    <div class="progress-celebration" aria-hidden="true">
      <Show when={!props.reducedMotion}>
        <For each={particles()}>
          {(p) => (
            <span
              class="progress-celebration-particle"
              style={{
                "--pc-angle": `${p.angle}deg`,
                "--pc-distance": `${p.distance}px`,
                "--pc-delay": `${p.delay}ms`,
                "--pc-scale": String(p.scale),
              }}
            />
          )}
        </For>
      </Show>
      <span class="progress-celebration-check">
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <polyline points="20 6 9 17 4 12" />
        </svg>
      </span>
    </div>
  );
}
