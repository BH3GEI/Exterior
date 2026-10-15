import { useEffect, useMemo, useRef } from "react";
import {
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  Background,
  type Edge,
  type Node as RFNode,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useStore } from "./store";
import { ExNode } from "./GraphNode";
import { InputBar } from "./InputBar";
import { layout, visibleNodes, NODE_W } from "./graph";

const nodeTypes = { ex: ExNode };

function Canvas() {
  const dir = useStore((s) => s.dir);
  useEffect(() => {
    const onResize = () => {
      const d = window.innerHeight > window.innerWidth ? "TB" : "LR";
      if (d !== useStore.getState().dir) useStore.getState().setDir(d);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  const order = useStore((s) => s.order);
  const nodes = useStore((s) => s.nodes);
  const children = useStore((s) => s.children);
  const collapsed = useStore((s) => s.collapsed);
  const runs = useStore((s) => s.runs);
  const lastAdded = useStore((s) => s.lastAdded);
  const rf = useReactFlow();
  const followRef = useRef<string | null>(null);

  const vis = useMemo(
    () => visibleNodes(order, nodes, children, collapsed, runs),
    [order, nodes, children, collapsed, runs]
  );

  const { rfNodes, rfEdges } = useMemo(() => {
    const pos = layout(vis, dir);
    const rfNodes: RFNode[] = [...vis.values()].map((g) => ({
      id: g.id,
      type: "ex",
      position: pos.get(g.id)!,
      data: { dir },
      draggable: false,
    }));
    const rfEdges: Edge[] = [];
    for (const g of vis.values()) {
      for (const p of g.parents) {
        if (vis.has(p)) {
          rfEdges.push({
            id: `${p}->${g.id}`,
            source: p,
            target: g.id,
            type: "smoothstep",
          });
        }
      }
    }
    return { rfNodes, rfEdges };
  }, [vis, dir]);

  // 新节点出现时视图跟随过去（SPEC §4.2）
  useEffect(() => {
    const last = order[order.length - 1] ?? `p:${Object.keys(runs)[0] ?? ""}`;
    if (!last || followRef.current === last) return;
    followRef.current = last;
    const n = rfNodes.find((x) => x.id === last) ?? rfNodes[rfNodes.length - 1];
    if (n) {
      const zoom = rf.getZoom();
      rf.setCenter(n.position.x + NODE_W / 2, n.position.y + 60, {
        zoom,
        duration: 200,
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lastAdded, rfNodes.length]);

  return (
    <ReactFlow
      nodes={rfNodes}
      edges={rfEdges}
      nodeTypes={nodeTypes}
      onNodeClick={(e, n) => {
        if (!n.id.startsWith("p:")) {
          useStore.getState().clickNode(n.id, e.shiftKey);
        }
      }}
      onPaneClick={() => useStore.getState().clearSelection()}
      selectionKeyCode={null}
      multiSelectionKeyCode={null}
      deleteKeyCode={null}
      nodesDraggable={false}
      nodesConnectable={false}
      minZoom={0.2}
      maxZoom={2}
      proOptions={{ hideAttribution: true }}
    >
      <Background gap={24} size={1} />
    </ReactFlow>
  );
}

export default function App() {
  const ready = useStore((s) => s.ready);
  const init = useStore((s) => s.init);
  useEffect(() => {
    init();
  }, [init]);
  if (!ready) return <div className="loading">…</div>;
  return (
    <ReactFlowProvider>
      <div className="app">
        <Canvas />
        <InputBar />
      </div>
    </ReactFlowProvider>
  );
}
