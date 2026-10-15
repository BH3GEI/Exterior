import { create } from "zustand";
import { immer } from "zustand/middleware/immer";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Ev, ModelCfg, NodeT, StateDto } from "./types";

export interface Pending {
  run: string;
  kind: "assistant" | "bash";
  parentId: string;
  text: string;
}

interface S {
  ready: boolean;
  nodes: Record<string, NodeT>;
  order: string[];
  children: Record<string, string[]>;
  collapsed: Record<string, boolean>;
  selection: string[]; // 有序，parents 按选中顺序
  models: ModelCfg[];
  model: string | null;
  error: string | null;
  runs: Record<string, { last: string; pending?: Pending }>;
  lastAdded: number; // 每次节点变化 +1，用于视图跟随
  dir: "LR" | "TB"; // 布局方向：宽窗口 LR，窄窗口 TB（SPEC §4.2）
  setDir: (d: "LR" | "TB") => void;
  init: () => Promise<void>;
  apply: (ev: Ev) => void;
  toggleCollapse: (id: string) => void;
  clickNode: (id: string, shift: boolean) => void;
  clearSelection: () => void;
  submit: (text: string) => Promise<void>;
  abortRun: (run: string) => void;
  chooseModel: (name: string) => void;
}

function insertNode(s: S, node: NodeT) {
  if (s.nodes[node.id]) return; // 幂等：重复事件不重复插入
  s.nodes[node.id] = node;
  s.order.push(node.id);
  for (const p of node.parents) {
    (s.children[p] ??= []).push(node.id);
  }
  s.children[node.id] ??= [];
}

let inited = false;

export const useStore = create<S>()(immer((set, get) => ({
  ready: false,
  nodes: {},
  order: [],
  children: {},
  collapsed: {},
  selection: [],
  models: [],
  model: null,
  error: null,
  runs: {},
  lastAdded: 0,
  dir: (typeof window !== "undefined" && window.innerHeight > window.innerWidth
    ? "TB"
    : "LR") as "LR" | "TB",
  setDir: (d) => set((s) => { s.dir = d; }),

  init: async () => {
    if (inited) return;
    inited = true;
    const dto = await invoke<StateDto>("get_state");
    set((s) => {
      for (const n of dto.nodes) insertNode(s, n);
      for (const c of dto.view.collapsed) s.collapsed[c] = true;
      s.models = dto.config?.models ?? [];
      s.model =
        dto.view.last_model ?? dto.config?.default ?? s.models[0]?.name ?? null;
      s.error = dto.config_error;
      s.ready = true;
    });
    await listen<Ev>("exterior-event", (e) => get().apply(e.payload));
  },

  apply: (ev) =>
    set((s) => {
      if (ev.type === "node") {
        insertNode(s, ev.node);
        const r = (s.runs[ev.run] ??= { last: ev.node.id });
        r.last = ev.node.id;
        delete r.pending;
        // 自动前进：当前选中的正是新节点的唯一直接来路 → 选中它，接着想
        if (s.selection.length === 1 && ev.node.parents.length === 1 &&
            ev.node.parents[0] === s.selection[0]) {
          s.selection = [ev.node.id];
        }
        s.lastAdded++;
      } else if (ev.type === "delta" || ev.type === "bash") {
        const r = (s.runs[ev.run] ??= { last: "" });
        const kind = ev.type === "delta" ? "assistant" : "bash";
        const chunk = ev.type === "delta" ? ev.text : ev.chunk;
        if (r.pending && r.pending.kind === kind) {
          r.pending = { ...r.pending, parentId: r.last, text: r.pending.text + chunk };
        } else {
          r.pending = { run: ev.run, kind, parentId: r.last, text: chunk };
        }
        s.lastAdded++;
      } else if (ev.type === "run_end") {
        delete s.runs[ev.run];
        s.lastAdded++;
      }
    }),

  toggleCollapse: (id) => {
    const next = !get().collapsed[id];
    set((s) => {
      if (next) s.collapsed[id] = true;
      else delete s.collapsed[id];
    });
    invoke("set_collapsed", { id, collapsed: next }).catch(console.error);
  },

  clickNode: (id, shift) => {
    const s = get();
    const n = s.nodes[id];
    if (!n) return;
    // SPEC §4.3：带 tool_call 的 assistant 不能作为来路 → 改为选中它的 bash 子节点
    if (n.kind === "assistant" && n.tool_call) {
      const bashKids = (s.children[id] ?? []).filter(
        (k) => s.nodes[k]?.kind === "bash"
      );
      if (bashKids.length === 0) return;
      id = bashKids[bashKids.length - 1];
    }
    set((st) => {
      if (shift) {
        if (st.selection.includes(id))
          st.selection = st.selection.filter((x) => x !== id);
        else st.selection = [...st.selection, id];
      } else {
        st.selection = [id];
      }
    });
  },

  clearSelection: () => set({ selection: [] }),

  submit: async (text) => {
    const { selection, model } = get();
    try {
      const run = await invoke<string>("think", {
        parents: selection,
        text,
        model,
      });
      set({ selection: [run], error: null });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  abortRun: (run) => {
    invoke("abort", { run }).catch(console.error);
  },

  chooseModel: (name) => {
    set({ model: name });
    invoke("set_last_model", { model: name }).catch(console.error);
  },
})));
