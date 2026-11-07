//! 设置持久化：settings.json（位于系统 AppData 下，跨平台路径由 Tauri 提供）。
//! 关键逻辑放 Rust 端：前端只负责展示与收集，读写全部走命令。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// 模型下载目录
    pub models_dir: String,
    /// HF 基址：直连官网或镜像
    pub mirror: String,
    /// 本地推理服务端口（后续 llama-server 用）
    pub port: u16,
    /// 并行下载数
    pub parallel_downloads: u8,
    /// 界面主题：dark / light / sepia
    pub theme: String,
    /// 上下文长度（tokens，-c 参数；越长 KV 缓存占内存越大）
    pub ctx_len: u32,
    /// 思考模式（仅对支持 reasoning 的模型生效）
    pub reasoning: bool,
    /// 局域网访问（对外开放 OpenAI 兼容 API）
    pub lan_access: bool,
    /// 手动导入的本地 GGUF 模型绝对路径列表（不移动原文件）
    pub extra_models: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            models_dir: default_models_dir(),
            mirror: "https://huggingface.co".to_string(),
            port: 8686,
            parallel_downloads: 2,
            theme: "dark".to_string(),
            ctx_len: 8192,
            reasoning: false,
            lan_access: false,
            extra_models: Vec::new(),
        }
    }
}

/// 默认模型目录：Windows 用 %USERPROFILE%\Models（兼容旧 D:\AI-Models），macOS/Linux 用 ~/Models
fn default_models_dir() -> String {
    #[cfg(target_os = "windows")]
    {
        // 新安装默认放到用户目录，避免没有 D: 盘的机器得到一个不可用路径。
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return std::path::Path::new(&profile)
                .join("Models")
                .display()
                .to_string();
        }
        "D:\\AI-Models".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(|h| {
                std::path::Path::new(&h)
                    .join("Models")
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|_| "~/Models".to_string())
    }
}

pub fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("settings.json")
}

/// 启动时加载（文件不存在或损坏则回退默认值，不报错）
pub fn load(app: &AppHandle) -> Settings {
    let p = settings_path(app);
    fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = settings_path(app);
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    fs::write(&p, json).map_err(|e| format!("写入配置失败: {e}"))
}

// ---------- Tauri 命令 ----------

#[tauri::command]
pub fn get_settings(state: tauri::State<'_, crate::AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    // 轻量校验：目录/镜像非空，端口与并行数在合理范围
    let mut s = settings;
    if s.models_dir.trim().is_empty() {
        s.models_dir = Settings::default().models_dir;
    }
    if s.mirror.trim().is_empty() {
        s.mirror = Settings::default().mirror;
    }
    s.mirror = s.mirror.trim().trim_end_matches('/').to_string();
    s.port = s.port.clamp(1024, 65535);
    s.parallel_downloads = s.parallel_downloads.clamp(1, 8);
    if !["dark", "light", "sepia"].contains(&s.theme.as_str()) {
        s.theme = "dark".to_string();
    }
    s.ctx_len = s.ctx_len.clamp(512, 262144);
    // reasoning / lan_access 是 bool，无需额外校验

    save(&app, &s)?;
    *state.settings.lock().unwrap() = s.clone();
    Ok(s)
}
