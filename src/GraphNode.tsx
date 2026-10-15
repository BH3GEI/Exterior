import { memo, useState } from "react";
import { Handle, Position } from "@xyflow/react";
import { useStore } from "./store";
import { BashView } from "./BashView";
import type { Dir } from "./graph";

const MAX_EXPANDED_TEXT = 4000;

function ExitBadge({ code }: { code?: number }) {
  if (code === undefined) return null;
  const cls = code === 0 ? "ok" : code === -1 ? "aborted" : "fail";
  const label = code === -1 ? "中止" : `exit ${code}`;
  return <span className={`badge ${cls}`}>{label}</span>;
}

export const ExNode = memo(function ExNode({
  id,
  data,
}: {
  id: string;
  data: { dir: Dir };
}) {
  const node = useStore((s) => s.nodes[id]);
  const pending = useStore((s) =>
    id.startsWith("p:") ? s.runs[id.slice(2)]?.pending : undefined
  );
  const run = id.startsWith("p:") ? id.slice(2) : null;
  const selected = useStore((s) => s.selection.includes(id));
  const selOrder = useStore((s) => s.selection.indexOf(id));
  const isCollapsed = useStore((s) => !!s.collapsed[id]);
  const descendants = useStore((s) => {
    let count = 0;
    const stack = [...(s.children[id] ?? [])];
    const seen = new Set<string>();
    while (stack.length) {
      const c = stack.pop()!;
      if (seen.has(c)) continue;
      seen.add(c);
      count++;
      stack.push(...(s.children[c] ?? []));
    }
    return count;
  });
  const { toggleCollapse, abortRun } = useStore.getState();
  const [expanded, setExpanded] = useState(false); // bash 卡片自身的展开（SPEC §4.4）

  const dir = data.dir;
  const tPos = dir === "LR" ? Position.Left : Position.Top;
  const sPos = dir === "LR" ? Position.Right : Position.Bottom;

  let cls = "ex-node";
  let body;
  if (pending) {
    cls += pending.kind === "bash" ? " kind-bash pending" : " kind-assistant pending";
    body = (
      <>
        <div className="head">
          <span className="kind">{pending.kind === "bash" ? "bash" : "…"}</span>
          <button
            className="stop"
            data-testid={`stop-${run}`}
            onClick={(e) => { e.stopPropagation(); abortRun(run!); }}
          >
            ■ 停止
          </button>
        </div>
        {pending.kind === "bash" ? (
          <pre className="bash-out">{pending.text}</pre>
        ) : (
          <div className="text">
            {pending.text}
            <span className="cursor">▌</span>
          </div>
        )}
      </>
    );
  } else if (node) {
    cls += ` kind-${node.kind}`;
    if (node.kind === "bash") {
      const out = node.output ?? "";
      const preview = out.split("\n").slice(0, 3).join("\n");
      body = (
        <>
          <div className="head">
            <span className="kind">$</span>
            <code className="cmd">{node.cmd}</code>
            <ExitBadge code={node.exit_code} />
          </div>
          {expanded ? (
            <>
              <BashView cmd={node.cmd ?? ""} output={out} />
              <button className="link" data-testid={`bash-collapse-${id}`}
                onClick={(e) => { e.stopPropagation(); setExpanded(false); }}>
                收起
              </button>
            </>
          ) : (
            out && (
              <pre
                className="bash-out preview"
                data-testid={`bash-expand-${id}`}
                onClick={(e) => { e.stopPropagation(); setExpanded(true); }}
              >
                {preview}
                {out.split("\n").length > 3 ? "\n…" : ""}
              </pre>
            )
          )}
        </>
      );
    } else {
      const text = node.text ?? "";
      const long = text.length > MAX_EXPANDED_TEXT;
      body = (
        <>
          <div className="head">
            <span className="kind">{node.kind === "user" ? "我" : "外"}</span>
            {node.model && <span className="model" data-testid={`model-${id}`}>{node.model}</span>}
          </div>
          <div className="text">
            {long && !expanded ? text.slice(0, MAX_EXPANDED_TEXT) + "…" : text}
            {node.tool_call && (
              <div className="tool-call">→ bash: <code>{node.tool_call.cmd}</code></div>
            )}
          </div>
          {long && (
            <button className="link" onClick={(e) => { e.stopPropagation(); setExpanded(!expanded); }}>
              {expanded ? "收起" : "展开"}
            </button>
          )}
        </>
      );
    }
  }

  return (
    <div className={`${cls}${selected ? " selected" : ""}`} data-testid={`node-${id}`}>
      <Handle type="target" position={tPos} />
      {selected && <span className="sel-order" data-testid={`sel-${id}`}>{selOrder + 1}</span>}
      {node && descendants > 0 && (
        <button
          className="collapse-btn"
          data-testid={`collapse-${id}`}
          title={isCollapsed ? `展开 ${descendants} 个节点` : "折叠"}
          onClick={(e) => { e.stopPropagation(); toggleCollapse(id); }}
        >
          {isCollapsed ? `⊞ ${descendants}` : "⊟"}
        </button>
      )}
      {body}
      <Handle type="source" position={sPos} />
    </div>
  );
});
