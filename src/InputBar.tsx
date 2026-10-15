import { useState } from "react";
import { useStore } from "./store";

export function InputBar() {
  const [text, setText] = useState("");
  const selection = useStore((s) => s.selection);
  const models = useStore((s) => s.models);
  const model = useStore((s) => s.model);
  const error = useStore((s) => s.error);
  const nodes = useStore((s) => s.nodes);
  const children = useStore((s) => s.children);
  const order = useStore((s) => s.order);
  const { submit, chooseModel } = useStore.getState();

  // 最近活跃的几个末端节点（SPEC §4.1）
  const leaves = order
    .filter((id) => (children[id] ?? []).length === 0)
    .sort((a, b) => b.localeCompare(a))
    .slice(0, 5);

  const send = () => {
    const t = text.trim();
    if (!t) return;
    setText("");
    submit(t);
  };

  return (
    <div className="input-bar">
      {error && <div className="error" data-testid="error">{error}</div>}
      {selection.length === 0 && leaves.length > 0 && (
        <div className="leaves" data-testid="leaves">
          <span className="hint">最近的末端：</span>
          {leaves.map((id) => {
            const n = nodes[id];
            const label =
              (n.kind === "bash" ? "$ " + (n.cmd ?? "") : (n.text ?? n.tool_call?.cmd ?? ""))
                .replace(/\s+/g, " ")
                .slice(0, 30);
            return (
              <button
                key={id}
                className={`chip chip-${n.kind}`}
                data-testid={`leaf-${id}`}
                onClick={() => useStore.getState().clickNode(id, false)}
              >
                {label || "…"}
              </button>
            );
          })}
        </div>
      )}
      <div className="row">
        <select
          className="model-select"
          data-testid="model-select"
          value={model ?? ""}
          onChange={(e) => chooseModel(e.target.value)}
        >
          {models.map((m) => (
            <option key={m.name} value={m.name}>{m.name}</option>
          ))}
          {models.length === 0 && <option value="">（无模型）</option>}
        </select>
        <textarea
          className="input"
          data-testid="input"
          placeholder={selection.length > 1 ? `合并 ${selection.length} 条来路…` : "想点什么…"}
          value={text}
          rows={1}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            }
          }}
        />
        <button className="send" data-testid="send" onClick={send}>↵</button>
      </div>
    </div>
  );
}
