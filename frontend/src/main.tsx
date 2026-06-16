import { render } from "solid-js/web";
import App from "./App";
import "./theme-bootstrap";
import "./index.css";
import { disposeAllRootMemos } from "./utils/reactive";
import { I18nProvider, i18n } from "./i18n";

const root = document.getElementById("root");
if (!root) throw new Error("Root element not found");

render(
  () => (
    <I18nProvider i18n={i18n}>
      <App />
    </I18nProvider>
  ),
  root,
);

// HMR 热替换时清理模块级 root memo,避免反应式计算图泄漏
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    disposeAllRootMemos();
  });
}
