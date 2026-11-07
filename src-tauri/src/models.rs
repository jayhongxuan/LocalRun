//! 模型推荐 + HuggingFace 直连搜索/文件列表。
//! 全部逻辑在 Rust 端：推荐规则、HF API 请求、字段裁剪，前端只做展示。

use crate::hardware::HardwareInfo;
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::State;

// ---------- 模型推荐 ----------

#[derive(Serialize, Clone)]
pub struct ModelRec {
    pub name: String,
    pub repo: String,
    pub quant: String,
    pub vram_gb: f32,
    pub ram_gb: f32,
    pub size_gb: f32,
    pub tier: u8, // 1=完整 GPU 加载 2=CPU/GPU 混合
    pub tier_label: String,
    pub note: String,
}

struct RecSeed {
    name: &'static str,
    repo: &'static str,
    quant: &'static str,
    vram_gb: f32,
    ram_gb: f32,
    size_gb: f32,
    note: &'static str,
}

/// 常用 GGUF 模型池（Q4_K_M 为主，尺寸为近似值）
const POOL: &[RecSeed] = &[
    RecSeed {
        name: "Qwen2.5-0.5B-Instruct",
        repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 0.6,
        ram_gb: 1.5,
        size_gb: 0.5,
        note: "最轻量，老设备也能跑",
    },
    RecSeed {
        name: "Qwen2.5-1.5B-Instruct",
        repo: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 1.3,
        ram_gb: 2.5,
        size_gb: 1.2,
        note: "轻量全能，中文好",
    },
    RecSeed {
        name: "DeepSeek-R1-Distill-Qwen-1.5B",
        repo: "deepseek-ai/DeepSeek-R1-Distill-Qwen-1.5B-GGUF",
        quant: "Q4_K_M",
        vram_gb: 1.3,
        ram_gb: 2.5,
        size_gb: 1.2,
        note: "轻量推理特化",
    },
    RecSeed {
        name: "Llama-3.2-3B-Instruct",
        repo: "meta-llama/Llama-3.2-3B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 2.5,
        ram_gb: 4.5,
        size_gb: 2.2,
        note: "英文能力均衡",
    },
    RecSeed {
        name: "Qwen2.5-3B-Instruct",
        repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 2.5,
        ram_gb: 4.5,
        size_gb: 2.0,
        note: "小尺寸中文首选",
    },
    RecSeed {
        name: "Phi-3.5-mini-instruct",
        repo: "microsoft/Phi-3.5-mini-instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 3.0,
        ram_gb: 5.0,
        size_gb: 2.3,
        note: "微软小模型，代码/数学强",
    },
    RecSeed {
        name: "DeepSeek-R1-Distill-Qwen-7B",
        repo: "deepseek-ai/DeepSeek-R1-Distill-Qwen-7B-GGUF",
        quant: "Q4_K_M",
        vram_gb: 5.0,
        ram_gb: 8.0,
        size_gb: 4.7,
        note: "6GB 卡完整加载，推理最强",
    },
    RecSeed {
        name: "Qwen2.5-7B-Instruct",
        repo: "Qwen/Qwen2.5-7B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 5.0,
        ram_gb: 8.0,
        size_gb: 4.7,
        note: "7B 中文标杆",
    },
    RecSeed {
        name: "Llama-3.1-8B-Instruct",
        repo: "meta-llama/Llama-3.1-8B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 5.4,
        ram_gb: 9.0,
        size_gb: 4.9,
        note: "8B 综合最强",
    },
    RecSeed {
        name: "Qwen2.5-7B-Instruct",
        repo: "Qwen/Qwen2.5-7B-Instruct-GGUF",
        quant: "Q8_0",
        vram_gb: 8.5,
        ram_gb: 12.0,
        size_gb: 8.1,
        note: "7B 高精度版，需混合加载",
    },
    RecSeed {
        name: "DeepSeek-R1-Distill-Qwen-14B",
        repo: "deepseek-ai/DeepSeek-R1-Distill-Qwen-14B-GGUF",
        quant: "Q4_K_M",
        vram_gb: 9.0,
        ram_gb: 14.0,
        size_gb: 8.9,
        note: "14B，需 CPU+GPU 混合",
    },
    RecSeed {
        name: "Qwen2.5-14B-Instruct",
        repo: "Qwen/Qwen2.5-14B-Instruct-GGUF",
        quant: "Q4_K_M",
        vram_gb: 9.0,
        ram_gb: 14.0,
        size_gb: 8.9,
        note: "14B 中文顶配",
    },
];

#[tauri::command]
pub fn recommend_models(hw: HardwareInfo) -> Vec<ModelRec> {
    let vram_gb = if hw.gpu.present {
        hw.gpu.vram_total_mb as f32 / 1024.0
    } else {
        0.0
    };
    let ram_gb = hw.ram_total_gb as f32;
    let disk_gb = hw.disk_free_gb as f32;

    let mut out: Vec<ModelRec> = Vec::new();
    for s in POOL {
        // 磁盘：模型本体 + 预留 5GB
        if s.size_gb > disk_gb - 5.0 {
            continue;
        }
        // 内存：模型 + 系统开销
        if s.ram_gb + 1.5 > ram_gb {
            continue;
        }
        let (tier, label) = if s.vram_gb + 0.5 <= vram_gb {
            (1u8, "完整 GPU 加载".to_string())
        } else if s.vram_gb <= ram_gb * 0.9 {
            (2u8, "CPU/GPU 混合（较慢）".to_string())
        } else {
            continue;
        };
        out.push(ModelRec {
            name: s.name.to_string(),
            repo: s.repo.to_string(),
            quant: s.quant.to_string(),
            vram_gb: s.vram_gb,
            ram_gb: s.ram_gb,
            size_gb: s.size_gb,
            tier,
            tier_label: label,
            note: s.note.to_string(),
        });
    }
    out.sort_by(|a, b| {
        a.tier.cmp(&b.tier).then(
            b.size_gb
                .partial_cmp(&a.size_gb)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    out.truncate(6);
    out
}

// ---------- HuggingFace 直连 ----------

#[derive(Serialize, Clone)]
pub struct HfModel {
    pub id: String,
    pub author: String,
    pub downloads: i64,
    pub likes: i64,
    pub last_modified: String,
    pub note: String,
}

#[derive(Deserialize)]
struct HfSearchItem {
    id: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    downloads: i64,
    #[serde(default)]
    likes: i64,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    gated: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct HfFile {
    pub path: String,
    pub size_bytes: i64,
}

#[derive(Deserialize)]
struct HfTreeItem {
    path: String,
    #[serde(rename = "type")]
    ftype: String,
    #[serde(default)]
    size: Option<i64>,
    #[serde(default)]
    lfs: Option<HfLfs>,
}

#[derive(Deserialize)]
struct HfLfs {
    #[serde(default)]
    size: Option<i64>,
}

#[derive(Serialize)]
pub struct NetStatus {
    pub base: String,
    pub ok: bool,
    pub ms: u64,
}

fn base_url(state: &AppState) -> String {
    state
        .settings
        .lock()
        .unwrap()
        .mirror
        .trim_end_matches('/')
        .to_string()
}

fn client(base: &str) -> Result<reqwest::Client, String> {
    if crate::net::mirror_use_direct(base) {
        crate::net::build_direct_client(Some(Duration::from_secs(30)))
    } else {
        crate::net::build_client(Some(Duration::from_secs(30)))
    }
}

fn short_ts(ts: &str) -> String {
    // 2026-10-29T01:23:45.000Z -> 2026-10-29
    ts.get(..10).unwrap_or(ts).to_string()
}

/// 搜索模型：按下载量倒序，附带 gguf 标签过滤
#[tauri::command]
pub async fn search_hf_models(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<HfModel>, String> {
    let base = base_url(&state);
    let client = client(&base)?;
    let q = query.trim();
    if q.is_empty() {
        return Err("请输入搜索关键词".to_string());
    }
    let url = format!("{base}/api/models");
    let params = [
        ("search", q),
        ("limit", "30"),
        ("sort", "downloads"),
        ("direction", "-1"),
        ("filter", "gguf"),
    ];
    let resp = client
        .get(&url)
        .query(&params)
        .send()
        .await
        .map_err(|e| format!("连接 {base} 失败（{e}）。可在设置里切换镜像。"))?;
    if !resp.status().is_success() {
        return Err(format!("搜索失败：HTTP {}", resp.status()));
    }
    let items: Vec<HfSearchItem> = resp
        .json()
        .await
        .map_err(|e| format!("解析响应失败: {e}"))?;
    Ok(items
        .into_iter()
        .map(|m| HfModel {
            id: m.id,
            author: m.author.unwrap_or_default(),
            downloads: m.downloads,
            likes: m.likes,
            last_modified: short_ts(m.last_modified.as_deref().unwrap_or("")),
            note: if m.gated.as_deref() == Some("auto") {
                "需登录授权".to_string()
            } else {
                String::new()
            },
        })
        .collect())
}

/// 取某个 repo 的文件树，过滤出 GGUF 文件
#[tauri::command]
pub async fn get_hf_files(
    state: State<'_, AppState>,
    repo_id: String,
) -> Result<Vec<HfFile>, String> {
    let base = base_url(&state);
    let client = client(&base)?;
    let repo = repo_id.trim().trim_start_matches('/');
    if repo.is_empty() {
        return Err("仓库地址为空".to_string());
    }
    let url = format!("{base}/api/models/{repo}/tree/main");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("连接 {base} 失败（{e}）。可在设置里切换镜像。"))?;
    if !resp.status().is_success() {
        return Err(format!("获取文件列表失败：HTTP {}", resp.status()));
    }
    let items: Vec<HfTreeItem> = resp.json().await.map_err(|e| format!("解析失败: {e}"))?;
    Ok(items
        .into_iter()
        .filter(|f| f.ftype == "file" && f.path.ends_with(".gguf"))
        .map(|f| HfFile {
            path: f.path,
            size_bytes: f.lfs.and_then(|l| l.size).or(f.size).unwrap_or(0),
        })
        .collect())
}

/// 连通性检测：状态栏显示直连是否可用
#[tauri::command]
pub async fn check_hf_connection(state: State<'_, AppState>) -> Result<NetStatus, String> {
    let base = base_url(&state);
    let t = std::time::Instant::now();
    let ok = match client(&base) {
        Ok(c) => c
            .get(format!("{base}/api/models"))
            .query(&[("limit", "1")])
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    };
    Ok(NetStatus {
        base,
        ok,
        ms: t.elapsed().as_millis() as u64,
    })
}
