import type { Meta, StoryObj } from "storybook-solidjs-vite";
import { createJSXDecorator } from "storybook-solidjs-vite";
import StatusBar from "./StatusBar";

type Story = StoryObj<typeof StatusBar>;

const meta: Meta<typeof StatusBar> = {
  title: "App/StatusBar",
  component: StatusBar,
  tags: ["autodocs"],
  decorators: [
    createJSXDecorator((Story) => (
      <div style={{ width: "720px" }}>
        <Story />
      </div>
    )),
  ],
};

export default meta;

export const Idle: Story = {
  args: {
    isIdle: true,
    totalSpeed: 0,
    activeCount: 0,
    pausedCount: 0,
    totalCount: 0,
  },
};

export const Downloading: Story = {
  args: {
    isIdle: false,
    totalSpeed: 12_500_000,
    activeCount: 3,
    pausedCount: 1,
    totalCount: 12,
  },
};
