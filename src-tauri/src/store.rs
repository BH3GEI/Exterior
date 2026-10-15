use crate::node::Node;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct View {
    #[serde(default)]
    pub collapsed: Vec<String>,
    #[serde(default)]
    pub last_model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    pub model: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub models: Vec<ModelCfg>,
    #[serde(default)]
    pub default: Option<String>,
}

pub fn dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".exterior")
}

pub struct Store {
    pub nodes: HashMap<String, Node>,
    pub children: HashMap<String, Vec<String>>,
    pub order: Vec<String>,
}

impl Store {
    pub fn load() -> std::io::Result<Store> {
        let mut s = Store {
            nodes: HashMap::new(),
            children: HashMap::new(),
            order: Vec::new(),
        };
        let path = dir().join("nodes.jsonl");
        if path.exists() {
            for line in BufReader::new(fs::File::open(path)?).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Node>(&line) {
                    Ok(n) => s.insert(n),
                    Err(e) => eprintln!("skip bad line: {e}"),
                }
            }
        }
        Ok(s)
    }

    pub fn insert(&mut self, n: Node) {
        for p in &n.parents {
            self.children.entry(p.clone()).or_default().push(n.id.clone());
        }
        self.order.push(n.id.clone());
        self.nodes.insert(n.id.clone(), n);
    }

    pub fn append(&mut self, n: Node) -> std::io::Result<()> {
        fs::create_dir_all(dir())?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir().join("nodes.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(&n).unwrap())?;
        self.insert(n);
        Ok(())
    }
}

pub fn read_view() -> View {
    fs::read_to_string(dir().join("view.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_view(v: &View) -> std::io::Result<()> {
    fs::create_dir_all(dir())?;
    fs::write(dir().join("view.json"), serde_json::to_string_pretty(v)?)
}

pub fn read_config() -> Result<Config, String> {
    let path = dir().join("config.json");
    let s = fs::read_to_string(&path)
        .map_err(|_| format!("config.json 不存在：{}（请参考 SPEC §2.3 创建）", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("config.json 解析失败：{e}"))
}
