//! Exterior 桌面应用：内核（exterior profile）在 Tauri 进程内，GUI 是内核的客户端。

use exterior_kernel::agent::AgentManager;
use exterior_kernel::dag::{DagProjection, ExteriorConfig};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

struct AppState {
    agents: Arc<AgentManager>,
    dag: Arc<DagProjection>,
}

#[derive(Serialize)]
struct StateDto {
    nodes: Vec<exterior_kernel::dag::DagNode>,
    view: exterior_kernel::dag::View,
    config: Option<ExteriorConfig>,
    config_error: Option<String>,
}

#[tauri::command]
fn get_state(state: tauri::State<'_, Arc<AppState>>) -> StateDto {
    let nodes = state.dag.store(|s| s.order.iter().map(|id| s.nodes[id].clone()).collect());
    let view = state.dag.store(|s| s.view.lock().unwrap().clone());
    let (config, config_error) = match exterior_kernel::dag::read_exterior_config() {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    };
    StateDto { nodes, view, config, config_error }
}

#[tauri::command]
async fn think(
    state: tauri::State<'_, Arc<AppState>>,
    parents: Vec<String>,
    text: String,
    model: Option<String>,
) -> Result<String, String> {
    exterior_kernel::dag::think_and_run(&state.dag, &state.agents, parents, text, model)
}

#[tauri::command]
fn abort(state: tauri::State<'_, Arc<AppState>>, run: String) {
    state.agents.cancel(&run);
}

#[tauri::command]
fn set_collapsed(state: tauri::State<'_, Arc<AppState>>, id: String, collapsed: bool) -> Result<(), String> {
    state.dag.store_mut(|s| {
        let mut view = s.view.lock().unwrap();
        if collapsed {
            if !view.collapsed.contains(&id) {
                view.collapsed.push(id);
            }
        } else {
            view.collapsed.retain(|x| *x != id);
        }
        drop(view);
        s.save_view().map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn set_last_model(state: tauri::State<'_, Arc<AppState>>, model: String) -> Result<(), String> {
    state.dag.store_mut(|s| {
        s.view.lock().unwrap().last_model = Some(model);
        s.save_view().map_err(|e| e.to_string())
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let (kernel, agents) = tauri::async_runtime::block_on(exterior_kernel::boot("exterior"))
                .map_err(|e| format!("内核启动失败：{e}"))?;
            let dag: Arc<DagProjection> = kernel
                .get::<Arc<DagProjection>>("dag")
                .map(|a| (*a).clone())
                .ok_or("exterior profile 没有 dag 服务")?;
            // DAG 事件 → 前端（形状与 M1 前端事件一致）
            let handle: AppHandle = app.handle().clone();
            let mut rx = dag.events.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    let Ok(ev) = rx.recv().await else { continue };
                    let _ = handle.emit("exterior-event", serde_json::to_value(&ev).unwrap());
                }
            });
            app.manage(Arc::new(AppState { agents, dag }));
            Ok(())
        })
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
