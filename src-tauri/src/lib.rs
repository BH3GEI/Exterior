mod bash;
mod context;
mod llm;
mod node;
mod store;

use node::{Node, ToolCall};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::{read_config, read_view, write_view, Store, View};
use tauri::{AppHandle, Emitter};

struct AppState {
    store: Mutex<Store>,
    view: Mutex<View>,
    runs: Mutex<HashMap<String, Arc<AtomicBool>>>, // run_id(=user 节点 id) -> 取消标志
    http: reqwest::Client,
}

#[derive(Serialize)]
struct StateDto {
    nodes: Vec<Node>,
    view: View,
    config: Option<store::Config>,
    config_error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Ev {
    Node { run: String, node: Node },
    Delta { run: String, text: String },
    Bash { run: String, chunk: String },
    RunEnd { run: String },
}

fn emit(app: &AppHandle, ev: &Ev) {
    let _ = app.emit("exterior-event", ev);
}

#[tauri::command]
fn get_state(state: tauri::State<'_, Arc<AppState>>) -> StateDto {
    let store = state.store.lock().unwrap();
    let (config, config_error) = match read_config() {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    };
    StateDto {
        nodes: store.order.iter().map(|id| store.nodes[id].clone()).collect(),
        view: state.view.lock().unwrap().clone(),
        config,
        config_error,
    }
}

#[tauri::command]
fn think(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    parents: Vec<String>,
    text: String,
    model: Option<String>,
) -> Result<String, String> {
    let mut user = Node::new(parents, "user");
    user.text = Some(text);
    let run = user.id.clone();
    {
        let mut store = state.store.lock().unwrap();
        store.append(user.clone()).map_err(|e| e.to_string())?;
    }
    emit(&app, &Ev::Node { run: run.clone(), node: user });

    let cancel = Arc::new(AtomicBool::new(false));
    state.runs.lock().unwrap().insert(run.clone(), cancel.clone());
    let st = state.inner().clone();
    tauri::async_runtime::spawn(run_think(app, st, run.clone(), model, cancel));
    Ok(run)
}

async fn run_think(
    app: AppHandle,
    state: Arc<AppState>,
    run: String,
    model: Option<String>,
    cancel: Arc<AtomicBool>,
) {
    let err = think_inner(&app, &state, &run, model, &cancel).await.err();
    if let Some(e) = err {
        // 出错也落一个 assistant 节点，让错误留在来路里
        let last = last_node_of(&state, &run);
        let mut n = Node::new(vec![last], "assistant");
        n.text = Some(format!("[错误] {e}"));
        if let Ok(mut store) = state.store.lock() {
            let _ = store.append(n.clone());
        }
        emit(&app, &Ev::Node { run: run.clone(), node: n });
    }
    state.runs.lock().unwrap().remove(&run);
    emit(&app, &Ev::RunEnd { run });
}

fn last_node_of(state: &Arc<AppState>, run: &str) -> String {
    // run 链的末端：从 user 节点沿最新子节点走到底
    let store = state.store.lock().unwrap();
    let mut cur = run.to_string();
    loop {
        let next = store
            .children
            .get(&cur)
            .and_then(|c| c.iter().max_by(|a, b| a.cmp(b)).cloned());
        match next {
            Some(n) => cur = n,
            None => return cur,
        }
    }
}

async fn think_inner(
    app: &AppHandle,
    state: &Arc<AppState>,
    run: &str,
    model: Option<String>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let cfg = read_config()?;
    let mcfg = match &model {
        Some(name) => cfg
            .models
            .iter()
            .find(|m| &m.name == name)
            .ok_or_else(|| format!("config.json 里没有模型 {name}"))?,
        None => {
            let name = cfg.default.as_deref().unwrap_or("");
            cfg.models
                .iter()
                .find(|m| m.name == name)
                .or(cfg.models.first())
                .ok_or("config.json 里没有可用模型")?
        }
    };
    let api_key = mcfg
        .api_key_env
        .as_ref()
        .and_then(|k| std::env::var(k).ok());

    let mut current = run.to_string();
    loop {
        let messages = {
            let store = state.store.lock().unwrap();
            context::build_messages(&store, &current)
        };
        let app2 = app.clone();
        let run2 = run.to_string();
        let res = llm::stream_chat(
            &state.http,
            &mcfg.base_url,
            api_key.as_deref(),
            &mcfg.model,
            messages,
            cancel.clone(),
            move |text| emit(&app2, &Ev::Delta { run: run2.clone(), text: text.to_string() }),
        )
        .await?;

        let mut an = Node::new(vec![current.clone()], "assistant");
        if !res.text.is_empty() {
            an.text = Some(res.text.clone());
        }
        an.model = Some(mcfg.name.clone());
        an.tool_call = res.tool_calls.first().map(|t| ToolCall { id: t.id.clone(), cmd: t.cmd.clone() });
        {
            let mut store = state.store.lock().unwrap();
            store.append(an.clone()).map_err(|e| e.to_string())?;
        }
        emit(app, &Ev::Node { run: run.to_string(), node: an.clone() });

        if res.aborted || res.tool_calls.is_empty() {
            return Ok(());
        }

        let mut prev = an.id.clone();
        let mut aborted = res.aborted;
        for tc in &res.tool_calls {
            if aborted {
                break;
            }
            let app2 = app.clone();
            let run2 = run.to_string();
            let r = bash::run_bash(&tc.cmd, cancel.clone(), move |chunk| {
                emit(&app2, &Ev::Bash { run: run2.clone(), chunk: chunk.to_string() })
            })
            .await?;
            aborted = r.aborted;
            let mut bn = Node::new(vec![prev.clone()], "bash");
            bn.cmd = Some(tc.cmd.clone());
            bn.output = Some(r.output);
            bn.exit_code = Some(r.exit_code);
            bn.call_id = Some(tc.id.clone());
            {
                let mut store = state.store.lock().unwrap();
                store.append(bn.clone()).map_err(|e| e.to_string())?;
            }
            emit(app, &Ev::Node { run: run.to_string(), node: bn.clone() });
            prev = bn.id;
        }
        if aborted {
            return Ok(());
        }
        current = prev;
    }
}

#[tauri::command]
fn abort(state: tauri::State<'_, Arc<AppState>>, run: String) {
    if let Some(c) = state.runs.lock().unwrap().get(&run) {
        c.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn set_collapsed(state: tauri::State<'_, Arc<AppState>>, id: String, collapsed: bool) -> Result<(), String> {
    let mut view = state.view.lock().unwrap();
    if collapsed {
        if !view.collapsed.contains(&id) {
            view.collapsed.push(id);
        }
    } else {
        view.collapsed.retain(|x| *x != id);
    }
    write_view(&view).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_last_model(state: tauri::State<'_, Arc<AppState>>, model: String) -> Result<(), String> {
    let mut view = state.view.lock().unwrap();
    view.last_model = Some(model);
    write_view(&view).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let store = Store::load().expect("读取 nodes.jsonl 失败");
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        view: Mutex::new(read_view()),
        runs: Mutex::new(HashMap::new()),
        http: reqwest::Client::new(),
    });
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_state,
            think,
            abort,
            set_collapsed,
            set_last_model
        ])
        .run(tauri::generate_context!())
        .expect("error while running exterior");
}
