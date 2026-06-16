import type { Preview } from "storybook-solidjs-vite";
import { createJSXDecorator } from "storybook-solidjs-vite";
import { I18nProvider } from "solid-i18n";
import { i18n } from "../src/i18n";
import "../src/index.css";

const preview: Preview = {
  decorators: [
    createJSXDecorator((Story) => (
      <I18nProvider i18n={i18n}>
        <Story />
      </I18nProvider>
    )),
  ],
  parameters: {
    actions: { argTypesRegex: "^on[A-Z].*" },
    controls: {
      matchers: {
        color: /(background|color)$/i,
        date: /Date$/,
      },
    },
  },
};

export default preview;
