import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { ansiLine } from "./ansi";

const LINE_H = 18;
const VIEW_H = 320;
const VIRTUAL_THRESHOLD = 100;

/** 渲染器：输出 → 组件。按内容识别（SPEC §4.4） */
function renderLine(line: string, kind: Kind, i: number): ReactNode {
  if (kind === "ansi") return <>{ansiLine(line)}</>;
  if (kind === "diff") {
    const cls = line.startsWith("+") && !line.startsWith("+++")
      ? "diff-add"
      : line.startsWith("-") && !line.startsWith("---")
        ? "diff-del"
        : line.startsWith("@")
          ? "diff-hunk"
          : "";
    return <span className={cls}>{line}</span>;
  }
  return <>{line}</>;
}

type Kind = "ansi" | "json" | "diff" | "plain";

function detect(cmd: string, output: string): { kind: Kind; text: string } {
  const t = output.trim();
  if ((t.startsWith("{") || t.startsWith("["))) {
    try {
      return { kind: "json", text: JSON.stringify(JSON.parse(t), null, 2) };
    } catch { /* fall through */ }
  }
  if (
    /\bdiff\b/.test(cmd) ||
    /^(diff |@@ |\+\+\+ |--- )/m.test(output)
  ) {
    return { kind: "diff", text: output };
  }
  if (output.includes("\x1b[")) return { kind: "ansi", text: output };
  return { kind: "plain", text: output };
}

function Lines({ lines, kind }: { lines: string[]; kind: Kind }) {
  const [scroll, setScroll] = useState(0);
  if (lines.length <= VIRTUAL_THRESHOLD) {
    return (
      <pre className="bash-out" data-testid="bash-output">
        {lines.map((l, i) => (
          <div key={i} className="bash-line">{renderLine(l, kind, i) || " "}</div>
        ))}
      </pre>
    );
  }
  // 虚拟滚动：长输出不卡界面
  const start = Math.max(0, Math.floor(scroll / LINE_H) - 10);
  const count = Math.ceil(VIEW_H / LINE_H) + 20;
  const slice = lines.slice(start, start + count);
  return (
    <div
      className="bash-out bash-virtual"
      data-testid="bash-output"
      style={{ height: VIEW_H }}
      onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
    >
      <div style={{ height: lines.length * LINE_H, position: "relative" }}>
        {slice.map((l, i) => (
          <div
            key={start + i}
            className="bash-line"
            style={{ position: "absolute", top: (start + i) * LINE_H, height: LINE_H }}
          >
            {renderLine(l, kind, start + i) || " "}
          </div>
        ))}
      </div>
    </div>
  );
}

export function BashView({ cmd, output }: { cmd: string; output: string }) {
  const { kind, text } = useMemo(() => detect(cmd, output), [cmd, output]);
  const lines = useMemo(() => text.split("\n"), [text]);
  return <Lines lines={lines} kind={kind} />;
}
