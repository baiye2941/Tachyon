import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@solidjs/testing-library";
import { createStore } from "solid-js/store";
import MagnetTab from "../tabs/MagnetTab";
import type { ConfigDraft } from "../SettingsPanel";

vi.mock("../../../api/invoke", () => ({
  api: {
    updateConfig: vi.fn(),
    refreshTrackerSubscription: vi.fn(),
    getConfig: vi.fn(),
  },
}));

vi.mock("../../../stores/btProxyCoverageCache", () => ({
  getBtProxyCoverageResource: () => () => null,
}));

vi.mock("../../../stores/toast", () => ({
  addToast: vi.fn(),
}));

const BEST_URL = "https://cf.trackerslist.com/best.txt";
const ALL_URL = "https://cf.trackerslist.com/all.txt";

function makeDraft(overrides: Partial<ConfigDraft["magnet"]> = {}) {
  // eslint 规则要求 createStore 返回值就地解构,再作为元组返回
  const [draft, setDraft] = createStore<ConfigDraft>({
    maxConcurrentTasks: 3,
    download: {
      downloadDir: "downloads",
      maxConcurrentFragments: 8,
      maxRetries: 3,
      requestTimeoutSecs: 30,
      verifyChecksum: true,
      rateLimitBytesPerSec: null,
      proxy: null,
      ioStrategy: "standard",
    },
    connection: {
      maxConnectionsPerHost: 4,
      maxGlobalConnections: 32,
      keepAliveTimeoutSecs: 60,
      enableHttp2: true,
      enableQuic: false,
      connectTimeoutSecs: 10,
    },
    scheduler: { minFragmentSize: 1024, maxFragmentSize: 4096, ewmaAlpha: 0.3 },
    magnet: {
      enableDht: true,
      enableUpnp: true,
      trackers: ["udp://tracker.example:1337"],
      trackerSubscriptionEnabled: false,
      trackerSubscriptionUrl: BEST_URL,
      trackerSubscriptionLastUpdated: null,
      disableDhtPersistence: false,
      socksProxyUrl: null,
      peerConnectTimeoutSecs: 8,
      peerReadWriteTimeoutSecs: 10,
      forceTrackerIntervalSecs: 120,
      deferWritesUpToMb: 16,
      disableDhtWhenSocks: true,
      allowPrivatePeers: false,
      ...overrides,
    },
    hub: { sourceMode: "mirror" },
    notifications: { enabled: true },
    clipboard: { enableWatch: false, pollIntervalMs: 1000 },
  });
  return [draft, setDraft] as const;
}

describe("MagnetTab 订阅源区域", () => {
  afterEach(() => {
    cleanup();
  });

  it("订阅关闭:折叠为一行摘要,不渲染 URL 输入框", () => {
    const [draft, setDraft] = makeDraft({ trackerSubscriptionEnabled: false });
    const { container } = render(() => (
      <MagnetTab draft={draft} setDraft={setDraft} />
    ));

    expect(container.querySelector(".sub-collapsed")).not.toBeNull();
    expect(container.querySelector(".sub-card")).toBeNull();
    expect(container.querySelector("input.sub-url-input")).toBeNull();
  });

  it("订阅打开:渲染完整卡片(输入框 + 预设 chip + 刷新按钮)", () => {
    const [draft, setDraft] = makeDraft({ trackerSubscriptionEnabled: true });
    const { container } = render(() => (
      <MagnetTab draft={draft} setDraft={setDraft} />
    ));

    expect(container.querySelector(".sub-collapsed")).toBeNull();
    expect(container.querySelector(".sub-card")).not.toBeNull();
    expect(container.querySelector("input.sub-url-input")).not.toBeNull();
    expect(container.querySelectorAll(".chip").length).toBe(3);
  });

  it("当前订阅源对应的 chip 高亮,点击其他 chip 更新 draft", () => {
    const [draft, setDraft] = makeDraft({
      trackerSubscriptionEnabled: true,
      trackerSubscriptionUrl: BEST_URL,
    });
    const { container } = render(() => (
      <MagnetTab draft={draft} setDraft={setDraft} />
    ));

    const chips = Array.from(container.querySelectorAll<HTMLElement>(".chip"));
    expect(chips[0]?.classList.contains("chip--active")).toBe(true);

    const allChip = chips[2]!;
    fireEvent.click(allChip);
    expect(allChip.classList.contains("chip--active")).toBe(true);
    expect(container.querySelector<HTMLInputElement>("input.sub-url-input")?.value).toBe(ALL_URL);
  });
});
