// 隐藏控制台窗口（debug 构建也不弹黑窗，避免挡 GUI）
#![windows_subsystem = "windows"]

mod chat;
mod download;
mod engine;
mod hardware;
mod media;
mod models;
mod net;
mod paths;
mod runtime;
mod settings;
mod tools;

use std::sync::Mutex;
use tauri::Manager;

/// 全局状态：设置 + 下载管理器 + 推理运行时 + 全局 HTTP 客户端（系统代理，无总超时，供下载用）
pub struct AppState {
    pub settings: Mutex<settings::Settings>,
    pub downloads: download::DownloadManager,
    pub runtime: runtime::RuntimeManager,
    pub client: reqwest::Client,
}

fn main() {
    let client = net::build_client(None).expect("failed to build http client");

    tauri::Builder::default()
        .manage(AppState {
            settings: Mutex::new(settings::Settings::default()),
            downloads: download::DownloadManager::new(2),
            runtime: runtime::RuntimeManager::new(),
            client,
        })
        .manage(media::MediaManager::new())
        .setup(|app| {
            let loaded = settings::load(app.handle());
            *app.state::<AppState>().settings.lock().unwrap() = loaded;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hardware::get_hardware_info,
            models::recommend_models,
            models::search_hf_models,
            models::get_hf_files,
            models::check_hf_connection,
            settings::get_settings,
            settings::save_settings,
            download::get_downloads,
            download::add_download,
            download::start_download,
            download::pause_download,
            download::resume_download,
            download::cancel_download,
            engine::install_engine,
            engine::get_engine_status,
            runtime::list_local_models,
            runtime::import_local_model,
            runtime::pick_gguf,
            runtime::start_model,
            runtime::stop_model,
            runtime::get_runtime_status,
            runtime::get_local_ip,
            media::get_media_settings,
            media::save_media_settings,
            media::pick_sd_cli,
            media::pick_media_file,
            media::get_media_engine_status,
            media::generate_image,
            media::generate_video,
            media::get_media_job_status,
            media::cancel_media_job,
            media::reveal_media_output,
            chat::chat_completion,
            tools::execute_tool,
            tools::read_attachment,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LocalRun");
}
