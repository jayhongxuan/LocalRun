//! 下载管理器：HuggingFace 模型文件断点续传下载。
//! 关键逻辑全在 Rust 端：Range 续传、并发槽、暂停/继续/取消、进度事件。
//! 临时文件以 .part 结尾，断点/暂停时保留，续传时按已有字节发 Range 请求。

use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DlStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Error,
    Cancelled,
}

#[derive(Serialize, Clone)]
pub struct DownloadTaskView {
    pub id: String,
    pub repo: String,
    pub file: String,
    pub total: u64,
    pub downloaded: u64,
    pub status: DlStatus,
    pub speed_bps: u64,
    pub error: Option<String>,
}

pub struct TaskInner {
    id: String,
    repo: String,
    file: String,
    url: String,
    part_path: PathBuf,
    dest_path: PathBuf,
    total: u64,
    downloaded: u64,
    status: DlStatus,
    speed_bps: u64,
    error: Option<String>,
}

impl TaskInner {
    fn view(&self) -> DownloadTaskView {
        DownloadTaskView {
            id: self.id.clone(),
            repo: self.repo.clone(),
            file: self.file.clone(),
            total: self.total,
            downloaded: self.downloaded,
            status: self.status.clone(),
            speed_bps: self.speed_bps,
            error: self.error.clone(),
        }
    }
}

type SharedTask = Arc<Mutex<TaskInner>>;

/// 诊断日志：写入跨平台应用数据目录/logs/dl_log.txt（本机网络问题排查用）。
fn dl_log(msg: &str) {
    let p = crate::paths::app_data_dir().join("logs").join("dl_log.txt");
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "[{}s] {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            msg
        );
    }
}

/// 防止 HF 返回的仓库/文件路径通过 ..、绝对路径或 Windows 盘符逃出 models_dir。
fn validate_download_path(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains('\0')
        || value.contains(':')
    {
        return Err(format!("{label}路径无效：{value}"));
    }
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("{label}路径无效：{value}"));
        }
    }
    Ok(())
}

pub struct DownloadManager {
    tasks: Arc<Mutex<HashMap<String, SharedTask>>>,
    sem: Arc<tokio::sync::Semaphore>,
}

impl DownloadManager {
    pub fn new(parallel: usize) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            sem: Arc::new(tokio::sync::Semaphore::new(parallel.max(1))),
        }
    }

    pub fn snapshot(&self) -> Vec<DownloadTaskView> {
        let mut v: Vec<DownloadTaskView> = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .map(|t| t.lock().unwrap().view())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// 加入队列（若同 repo+file 已有未完成任务则复用并返回其 id）
    pub fn add(
        &self,
        base: &str,
        repo: &str,
        file: &str,
        size: i64,
        dir: &str,
    ) -> Result<String, String> {
        validate_download_path(repo, "仓库")?;
        validate_download_path(file, "文件")?;
        {
            let tasks = self.tasks.lock().unwrap();
            for t in tasks.values() {
                let t = t.lock().unwrap();
                if t.repo == repo
                    && t.file == file
                    && t.status != DlStatus::Completed
                    && t.status != DlStatus::Cancelled
                {
                    return Ok(t.id.clone());
                }
            }
        }
        let id = format!(
            "dl_{}_{}",
            file.replace([' ', '.', '/'], "_"),
            uuid::Uuid::new_v4().simple()
        );
        let repo_dir = repo.replace('/', "__");
        let dest_dir = PathBuf::from(dir).join(&repo_dir);
        fs::create_dir_all(&dest_dir).map_err(|e| format!("创建目录失败: {e}"))?;
        let dest_path = dest_dir.join(file);
        let part_path = dest_dir.join(format!("{file}.part"));
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建模型子目录失败: {e}"))?;
        }
        let url = format!("{base}/{repo}/resolve/main/{file}");

        let task = Arc::new(Mutex::new(TaskInner {
            id: id.clone(),
            repo: repo.to_string(),
            file: file.to_string(),
            url: url.clone(),
            part_path,
            dest_path,
            total: size.max(0) as u64,
            downloaded: 0,
            status: DlStatus::Queued,
            speed_bps: 0,
            error: None,
        }));
        dl_log(&format!("add task {id} repo={repo} file={file} url={url}"));
        self.tasks.lock().unwrap().insert(id.clone(), task);
        Ok(id)
    }

    pub fn spawn_worker(
        &self,
        app: AppHandle,
        id: &str,
        client: reqwest::Client,
    ) -> Result<(), String> {
        let task = self.tasks.lock().unwrap().get(id).cloned();
        let Some(task) = task else {
            return Err("任务不存在".to_string());
        };
        {
            let mut t = task.lock().unwrap();
            if t.status == DlStatus::Completed || t.status == DlStatus::Cancelled {
                return Ok(());
            }
            if t.status == DlStatus::Downloading {
                return Ok(()); // 已在下载
            }
            t.status = DlStatus::Queued;
            t.error = None;
        }
        let sem = self.sem.clone();
        let task2 = task.clone();
        let wid = id.to_string();
        tauri::async_runtime::spawn(async move {
            dl_log(&format!("worker enter {wid}"));
            // 等待并发槽
            let _perm = sem.acquire().await.ok();
            dl_log(&format!("worker got permit {wid}"));
            // 槽位到手后复查状态（可能期间被暂停/取消）
            let st = task2.lock().unwrap().status.clone();
            dl_log(&format!("worker state {wid} = {st:?}"));
            if st != DlStatus::Queued {
                return;
            }
            run_download(app, task2, client).await;
        });
        dl_log(&format!("spawn_worker {id}"));
        Ok(())
    }

    pub fn pause(&self, id: &str) -> Result<(), String> {
        let tasks = self.tasks.lock().unwrap();
        let t = tasks.get(id).ok_or("任务不存在")?;
        let mut t = t.lock().unwrap();
        if t.status == DlStatus::Downloading || t.status == DlStatus::Queued {
            t.status = DlStatus::Paused;
        }
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let tasks = self.tasks.lock().unwrap();
        let t = tasks.get(id).ok_or("任务不存在")?;
        {
            let mut t = t.lock().unwrap();
            t.status = DlStatus::Cancelled;
            t.error = None;
        }
        // 删除 part（若正在写，等待其退出；删失败也无妨，续传会跳过）
        let _ = fs::remove_file(&t.lock().unwrap().part_path);
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.cancel(id)?;
        self.tasks.lock().unwrap().remove(id);
        Ok(())
    }

    pub fn cleanup_completed(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        let done: Vec<String> = tasks
            .iter()
            .filter(|(_, t)| {
                let t = t.lock().unwrap();
                t.status == DlStatus::Completed || t.status == DlStatus::Cancelled
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in done {
            tasks.remove(&k);
        }
    }
}

fn emit_progress(app: &AppHandle, view: &DownloadTaskView) {
    let _ = app.emit("download-progress", view);
}

async fn run_download(app: AppHandle, task: SharedTask, client: reqwest::Client) {
    use futures_util::StreamExt;
    use reqwest::StatusCode;

    {
        let mut t = task.lock().unwrap();
        t.status = DlStatus::Downloading;
        t.speed_bps = 0;
        emit_progress(&app, &t.view());
    }

    // 断点：已有 part 大小即偏移
    let part_path = task.lock().unwrap().part_path.clone();
    let dest_path = task.lock().unwrap().dest_path.clone();
    let url = task.lock().unwrap().url.clone();

    let mut offset = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let initial_total = task.lock().unwrap().total;
    dl_log(&format!(
        "run start id={} offset={} url={}",
        task.lock().unwrap().id,
        offset,
        url
    ));
    if offset > 0 && initial_total > 0 && offset >= initial_total {
        // part 已完整 → 直接改名收尾
        let _ = fs::rename(&part_path, &dest_path);
        {
            let mut t = task.lock().unwrap();
            t.status = DlStatus::Completed;
            t.downloaded = t.total;
            emit_progress(&app, &t.view());
        }
        return;
    }

    loop {
        let st = {
            let t = task.lock().unwrap();
            t.status.clone()
        };
        if st == DlStatus::Paused || st == DlStatus::Cancelled {
            let view = task.lock().unwrap().view();
            emit_progress(&app, &view);
            return;
        }
        if st != DlStatus::Downloading {
            return;
        }

        let mut req = client.get(&url);
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                dl_log(&format!("{} conn error: {e}", task.lock().unwrap().id));
                let mut t = task.lock().unwrap();
                t.status = DlStatus::Error;
                t.error = Some(format!("连接失败: {e}"));
                emit_progress(&app, &t.view());
                return;
            }
        };
        dl_log(&format!(
            "{} resp status={}",
            task.lock().unwrap().id,
            resp.status()
        ));
        match resp.status() {
            StatusCode::PARTIAL_CONTENT | StatusCode::OK => {
                if resp.status() == StatusCode::OK && offset > 0 {
                    offset = 0; // 服务器不支持 Range，从头来
                }
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                // offset 已到文件尾（本地 part 完整）
                let _ = fs::rename(&part_path, &dest_path);
                let mut t = task.lock().unwrap();
                t.status = DlStatus::Completed;
                t.downloaded = t.total;
                emit_progress(&app, &t.view());
                return;
            }
            s => {
                let mut t = task.lock().unwrap();
                t.status = DlStatus::Error;
                t.error = Some(format!("下载失败: HTTP {s}"));
                emit_progress(&app, &t.view());
                return;
            }
        }

        let cl = resp.content_length().unwrap_or(0);
        {
            let mut t = task.lock().unwrap();
            if t.total == 0 || cl > 0 {
                t.total = offset + cl;
            }
        }

        // 追加写入 .part
        let mut f = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
        {
            Ok(f) => f,
            Err(e) => {
                let mut t = task.lock().unwrap();
                t.status = DlStatus::Error;
                t.error = Some(format!("写入文件失败: {e}"));
                emit_progress(&app, &t.view());
                return;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut last_update = Instant::now();
        let mut last_bytes = {
            let t = task.lock().unwrap();
            t.downloaded
        };

        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    // 网络中断：保留 part，标记错误供续传
                    let mut t = task.lock().unwrap();
                    t.status = DlStatus::Error;
                    t.error = Some(format!("传输中断: {e}"));
                    emit_progress(&app, &t.view());
                    return;
                }
            };
            if let Err(e) = f.write_all(&chunk) {
                let mut t = task.lock().unwrap();
                t.status = DlStatus::Error;
                t.error = Some(format!("写入失败: {e}"));
                emit_progress(&app, &t.view());
                return;
            }
            let now = Instant::now();
            let mut should_emit = false;
            {
                let mut t = task.lock().unwrap();
                t.downloaded += chunk.len() as u64;
                if now.duration_since(last_update).as_millis() >= 150 {
                    let dt = now.duration_since(last_update).as_secs_f64().max(0.001);
                    t.speed_bps = ((t.downloaded - last_bytes) as f64 / dt) as u64;
                    last_update = now;
                    last_bytes = t.downloaded;
                    should_emit = true;
                }
            }
            if should_emit {
                let view = task.lock().unwrap().view();
                emit_progress(&app, &view);
            }
            let st = task.lock().unwrap().status.clone();
            if st == DlStatus::Paused || st == DlStatus::Cancelled {
                // 暂停/取消：保留（或删除）part
                if st == DlStatus::Cancelled {
                    let _ = fs::remove_file(&part_path);
                }
                let view = task.lock().unwrap().view();
                emit_progress(&app, &view);
                return;
            }
        }
        drop(f);

        // 流正常结束
        let _ = fs::rename(&part_path, &dest_path);
        {
            let mut t = task.lock().unwrap();
            t.status = DlStatus::Completed;
            t.speed_bps = 0;
            emit_progress(&app, &t.view());
        }
        return;
    }
}

// ---------- Tauri 命令 ----------

#[tauri::command]
pub fn get_downloads(state: tauri::State<'_, crate::AppState>) -> Vec<DownloadTaskView> {
    state.downloads.snapshot()
}

#[tauri::command]
pub fn add_download(
    state: tauri::State<'_, crate::AppState>,
    repo_id: String,
    file: String,
    size: i64,
) -> Result<String, String> {
    let (base, dir) = {
        let s = state.settings.lock().unwrap().clone();
        (s.mirror, s.models_dir)
    };
    state.downloads.add(&base, &repo_id, &file, size, &dir)
}

#[tauri::command]
pub fn start_download(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    let client = pick_download_client(&state)?;
    state.downloads.spawn_worker(app, &id, client)
}

#[tauri::command]
pub fn pause_download(state: tauri::State<'_, crate::AppState>, id: String) -> Result<(), String> {
    state.downloads.pause(&id)
}

#[tauri::command]
pub fn resume_download(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    id: String,
) -> Result<(), String> {
    let client = pick_download_client(&state)?;
    state.downloads.spawn_worker(app, &id, client)
}

/// 按当前镜像选择下载客户端：国内镜像（hf-mirror 等）直连，HuggingFace 本体走系统代理
fn pick_download_client(
    state: &tauri::State<'_, crate::AppState>,
) -> Result<reqwest::Client, String> {
    let base = {
        let s = state.settings.lock().unwrap().clone();
        s.mirror
    };
    if crate::net::mirror_use_direct(&base) {
        crate::net::build_direct_client(None)
    } else {
        Ok(state.client.clone())
    }
}

#[tauri::command]
pub fn cancel_download(state: tauri::State<'_, crate::AppState>, id: String) -> Result<(), String> {
    state.downloads.cancel(&id)
}

#[cfg(test)]
mod tests {
    use super::validate_download_path;

    #[test]
    fn accepts_normal_hf_paths() {
        assert!(validate_download_path("owner/repo", "仓库").is_ok());
        assert!(validate_download_path("folder/model-Q4_K_M.gguf", "文件").is_ok());
    }

    #[test]
    fn rejects_path_escape_attempts() {
        for path in [
            "../evil.gguf",
            "/tmp/evil.gguf",
            "a/../../evil.gguf",
            "a//b.gguf",
            "C:\\\\temp\\\\evil.gguf",
            "a\\b.gguf",
            "",
        ] {
            assert!(validate_download_path(path, "文件").is_err(), "{path}");
        }
    }
}
