use crate::store::Store;
use serde_json::{json, Value};
use std::collections::HashSet;

const SYSTEM: &str = "你是用户思考的一部分，不是助手。你可以用 bash 访问整台电脑。所有过去的念头都在 ~/.exterior/nodes.jsonl 里，需要时自己去找。";

const MAX_BASH_OUTPUT: usize = 16_000;

/// 收集 id 及其全部祖先
fn subtree(store: &Store, id: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut stack = vec![id.to_string()];
    while let Some(cur) = stack.pop() {
        if set.insert(cur.clone()) {
            if let Some(n) = store.nodes.get(&cur) {
                stack.extend(n.parents.iter().cloned());
            }
        }
    }
    set
}

fn sort_ids(store: &Store, ids: &mut Vec<String>) {
    ids.sort_by(|a, b| a.cmp(b)); // ULID 按时间可排序，父一定先于子
    ids.retain(|id| store.nodes.contains_key(id));
}

/// SPEC §3.1：先共同部分，再按 parents 顺序逐条分支；合并处分支开头带 [来路 i/n]。
/// 返回 (node_id, Option<marker>) 序列，含 target 本身。
pub fn context_order(store: &Store, target: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    walk(store, target, &mut visited, &mut out);
    out
}

fn walk(
    store: &Store,
    id: &str,
    visited: &mut HashSet<String>,
    out: &mut Vec<(String, Option<String>)>,
) {
    let Some(node) = store.nodes.get(id) else { return };
    if node.parents.len() <= 1 {
        for p in &node.parents {
            if !visited.contains(p) {
                walk(store, p, visited, out);
            }
        }
    } else {
        let sets: Vec<HashSet<String>> =
            node.parents.iter().map(|p| subtree(store, p)).collect();
        let mut common = sets[0].clone();
        for s in &sets[1..] {
            common = common.intersection(s).cloned().collect();
        }
        let mut cv: Vec<String> = common.into_iter().collect();
        sort_ids(store, &mut cv);
        for c in cv {
            if visited.insert(c.clone()) {
                out.push((c, None));
            }
        }
        let n = node.parents.len();
        for i in 0..node.parents.len() {
            let mut bv: Vec<String> = sets[i]
                .iter()
                .filter(|x| !visited.contains(*x))
                .cloned()
                .collect();
            sort_ids(store, &mut bv);
            if bv.is_empty() {
                continue;
            }
            for (j, b) in bv.into_iter().enumerate() {
                visited.insert(b.clone());
                let marker = (j == 0).then(|| format!("[来路 {}/{}]", i + 1, n));
                out.push((b, marker));
            }
        }
    }
    if visited.insert(id.to_string()) {
        out.push((id.to_string(), None));
    }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_BASH_OUTPUT {
        return s.to_string();
    }
    let half = MAX_BASH_OUTPUT / 2;
    let head = &s[..floor_boundary(s, half)];
    let tail_start = floor_boundary(s, s.len().saturating_sub(half));
    let tail = &s[tail_start..];
    format!("{head}\n[...省略 {} 字符...]\n{tail}", s.len() - MAX_BASH_OUTPUT)
}

fn tool_call_json(id: &str, cmd: &str) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {"name": "bash", "arguments": json!({"command": cmd}).to_string()}
    })
}

/// SPEC §3.1：祖先 → OpenAI 消息序列
pub fn build_messages(store: &Store, target: &str) -> Vec<Value> {
    let mut msgs = vec![json!({"role": "system", "content": SYSTEM})];
    for (id, marker) in context_order(store, target) {
        if let Some(m) = marker {
            msgs.push(json!({"role": "user", "content": m}));
        }
        let Some(node) = store.nodes.get(&id) else { continue };
        match node.kind.as_str() {
            "user" => msgs.push(json!({"role": "user", "content": node.text.clone().unwrap_or_default()})),
            "assistant" => {
                let mut msg = json!({"role": "assistant", "content": node.text.clone().unwrap_or_default()});
                if let Some(tc) = &node.tool_call {
                    // 一次返回多个 tool call 时，后续的只有 bash 子节点留下 call_id，这里重建
                    let mut calls = vec![tool_call_json(&tc.id, &tc.cmd)];
                    let empty = Vec::new();
                    let kids = store.children.get(&node.id).unwrap_or(&empty);
                    for kid in kids {
                        if let Some(k) = store.nodes.get(kid) {
                            if k.kind == "bash" {
                                if let (Some(cid), Some(cmd)) = (&k.call_id, &k.cmd) {
                                    if *cid != tc.id {
                                        calls.push(tool_call_json(cid, cmd));
                                    }
                                }
                            }
                        }
                    }
                    msg["tool_calls"] = json!(calls);
                }
                msgs.push(msg);
            }
            "bash" => msgs.push(json!({
                "role": "tool",
                "tool_call_id": node.call_id.clone().unwrap_or_default(),
                "content": truncate_output(node.output.as_deref().unwrap_or_default()),
            })),
            _ => {}
        }
    }
    msgs
}
