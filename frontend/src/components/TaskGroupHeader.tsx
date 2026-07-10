import { ChevronDownIcon } from "./icons";
import { useI18n } from "../i18n";
import type { GroupKey } from "./taskGroups";

export interface TaskGroupHeaderProps {
  group: GroupKey;
  count: number;
  collapsed: boolean;
  isActive: boolean;
  height?: number;
  onToggle: () => void;
}

export default function TaskGroupHeader(props: TaskGroupHeaderProps) {
  const i18n = useI18n();

  const groupName = () => i18n.t(`taskList.group.${props.group}`) as string;

  const ariaLabel = () =>
    i18n.t("taskList.aria.groupHeader", {
      name: groupName(),
      count: props.count,
    }) as string;

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      props.onToggle();
    }
  };

  return (
    <div
      id={`task-group-header-${props.group}`}
      role="group"
      tabIndex={-1}
      aria-expanded={!props.collapsed}
      aria-label={ariaLabel()}
      class="task-group-header"
      classList={{
        "task-group-header--active": props.isActive,
        "task-group-header--collapsed": props.collapsed,
      }}
      style={props.height !== undefined ? { height: `${props.height}px` } : undefined}
      onClick={() => props.onToggle()}
      onKeyDown={handleKeyDown}
    >
      <ChevronDownIcon
        size={14}
        class={`task-group-header__chevron ${props.collapsed ? "task-group-header__chevron--collapsed" : ""}`}
      />
      <span class="task-group-header__name">{groupName()}</span>
      <span class="task-group-header__count">{props.count}</span>
    </div>
  );
}
