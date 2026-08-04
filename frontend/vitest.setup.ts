// jsdom 30 兼容补丁
//
// vitest populateGlobal 的 getWindowKeys 过滤规则:`if (k in global) return
// keysArray.includes(k)` —— globalThis 上已有 Node/bun 原生的 localStorage
// 惰性 getter(实验性 webstorage,默认 flag 关闭时返回 undefined),而
// localStorage 不在 vitest 的 KEYS 白名单中,于是 jsdom 窗口的真实实现被跳过,
// 测试环境拿到的 localStorage 是 Node 原生的 undefined。
//
// 这里从 vitest 注入的 `window.jsdom` 句柄取回 jsdom 真实 storage 并重新绑定。
// 仅在当前值是 undefined 时覆盖,不影响正常环境(如 jsdom 29 或 flag 已启用)。
const jsdomWindow = (globalThis as { jsdom?: { window: Window } }).jsdom?.window;
if (jsdomWindow && typeof globalThis.localStorage === "undefined") {
  Object.defineProperty(globalThis, "localStorage", {
    value: jsdomWindow.localStorage,
    configurable: true,
    writable: true,
  });
  Object.defineProperty(globalThis, "sessionStorage", {
    value: jsdomWindow.sessionStorage,
    configurable: true,
    writable: true,
  });
}
