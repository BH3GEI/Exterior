export interface ToolCall {
  id: string;
  cmd: string;
}

export interface NodeT {
  id: string;
  parents: string[];
  kind: "user" | "assistant" | "bash";
  text?: string;
  model?: string;
  tool_call?: ToolCall;
  cmd?: string;
  output?: string;
  exit_code?: number;
  call_id?: string;
  created_at: string;
}

export interface ModelCfg {
  name: string;
  base_url: string;
  api_key_env?: string;
  model: string;
}

export interface StateDto {
  nodes: NodeT[];
  view: { collapsed: string[]; last_model?: string | null };
  config: { models: ModelCfg[]; default?: string | null } | null;
  config_error: string | null;
}

export type Ev =
  | { type: "node"; run: string; node: NodeT }
  | { type: "delta"; run: string; text: string }
  | { type: "bash"; run: string; chunk: string }
  | { type: "run_end"; run: string };
