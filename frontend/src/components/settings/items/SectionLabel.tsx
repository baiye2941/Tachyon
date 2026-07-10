export interface SectionLabelProps {
  text: string;
}

/** 分区小标题(magnet tab 分组:Peer 超时 / 高级) */
export default function SectionLabel(props: SectionLabelProps) {
  return (
    <div
      style={{
        "font-size": "11px",
        "font-weight": 600,
        color: "var(--color-text-tertiary)",
        "text-transform": "uppercase",
        "letter-spacing": "0.5px",
        "margin-top": "4px",
        "margin-bottom": "-4px",
      }}
    >
      {props.text}
    </div>
  );
}
