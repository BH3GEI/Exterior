import dagre from "dagre";
import type { NodeT } from "./types";
import type { Pending } from "./store";

export type Dir = "LR" | "TB";
export const NODE_W = 340;

export interface GNode {
  id: string; // 真实节点 id 或 `p:${run}`（生成中）
  node?: NodeT;
  pending?: Pending;
  parents: string[];
}

/** SPEC §4.3：一个节点只有在所有来路都被收起时才隐藏 */
export function visibleNodes(
  order: string[],
  nodes: Record<string, NodeT>,
  children: Record<string, string[]>,
  collapsed: Record<string, boolean>,
  runs: Record<string, { last: string; pending?: Pending }>
): Map<string, GNode> {
  const vis = new Map<string, GNode>();
  for (const id of order) {
    const n = nodes[id];
    if (!n) continue;
    const show =
      n.parents.length === 0 ||
      n.parents.some((p) => vis.has(p) && !collapsed[p]);
    if (show) vis.set(id, { id, node: n, parents: n.parents.filter((p) => vis.has(p)) });
  }
  for (const [run, r] of Object.entries(runs)) {
    if (r.pending && vis.has(r.pending.parentId)) {
      vis.set(`p:${run}`, {
        id: `p:${run}`,
        pending: { ...r.pending },
        parents: [r.pending.parentId],
      });
    }
  }
  return vis;
}

export function estimateHeight(g: GNode): number {
  if (g.pending) return 90;
  const n = g.node!;
  if (n.kind === "bash") return 110;
  const text = n.text ?? n.tool_call?.cmd ?? "";
  const lines = Math.min(12, Math.ceil(text.length / 42) || 1);
  return 52 + lines * 20;
}

/** dagre 布局；返回 id -> 左上角坐标 */
export function layout(
  vis: Map<string, GNode>,
  dir: Dir
): Map<string, { x: number; y: number }> {
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: dir, nodesep: 24, ranksep: 90, marginx: 40, marginy: 40 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const [id, gn] of vis) {
    g.setNode(id, { width: NODE_W, height: estimateHeight(gn) });
  }
  for (const [id, gn] of vis) {
    for (const p of gn.parents) {
      if (vis.has(p)) g.setEdge(p, id);
    }
  }
  dagre.layout(g);
  const out = new Map<string, { x: number; y: number }>();
  for (const [id, gn] of vis) {
    const d = g.node(id);
    out.set(id, { x: d.x - NODE_W / 2, y: d.y - estimateHeight(gn) / 2 });
  }
  return out;
}
