import type { StorybookConfig } from "storybook-solidjs-vite";

const config: StorybookConfig = {
  stories: ["../src/**/*.stories.@(js|jsx|ts|tsx)"],
  framework: {
    name: "storybook-solidjs-vite",
    options: {
      docgen: false,
    },
  },
  async viteFinal(config) {
    config.build = config.build || {};
    config.build.chunkSizeWarningLimit = 1000;
    return config;
  },
};

export default config;
