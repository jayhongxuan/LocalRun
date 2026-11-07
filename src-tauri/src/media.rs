//! 本地媒体生成管理：通过官方 stable-diffusion.cpp `sd-cli` 执行图片及 Wan 视频任务。
//! 将本模块加入 crate 并在 Tauri Builder 中 `.manage(MediaManager::new())`，再注册下方命令即可。

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const LOG_TAIL_BYTES: u64 = 12 * 1024;

/// 本地媒体设置。路径均以平台原生 PathBuf 表达；输出位置不允许由调用方指定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MediaSettings {
    pub sd_cli_path: PathBuf,
    pub image_model: PathBuf,
    /// 分离式图片模型的 VAE（例如 Z-Image / Flux）；单文件 checkpoint 留空。
    pub image_vae: PathBuf,
    /// 分离式图片模型的 LLM 文本编码器（例如 Z-Image 的 Qwen3-4B）；单文件 checkpoint 留空。
    pub image_text_encoder: PathBuf,
    pub video_diffusion: PathBuf,
    pub video_vae: PathBuf,
    pub video_t5xxl: PathBuf,
    /// Wan2.1 I2V / FLF2V 需要的 CLIP Vision（官方参数：--clip_vision）
    pub video_clip_vision: PathBuf,
    /// Wan2.2 A14B 的 high-noise 扩散模型（官方参数：--high-noise-diffusion-model）
    pub video_high_noise_diffusion: PathBuf,
    /// wan21_t2v | wan21_i2v | wan22_ti2v | wan22_a14b
    pub video_model_kind: String,
    pub input_image: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub image_steps: u32,
    pub image_cfg_scale: f32,
    pub video_frames: u32,
    pub video_steps: u32,
    pub video_cfg_scale: f32,
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self {
            sd_cli_path: PathBuf::new(),
            image_model: PathBuf::new(),
            image_vae: PathBuf::new(),
            image_text_encoder: PathBuf::new(),
            video_diffusion: PathBuf::new(),
            video_vae: PathBuf::new(),
            video_t5xxl: PathBuf::new(),
            video_clip_vision: PathBuf::new(),
            video_high_noise_diffusion: PathBuf::new(),
            video_model_kind: "wan21_t2v".to_string(),
            input_image: None,
            width: 512,
            height: 512,
            image_steps: 20,
            image_cfg_scale: 7.0,
            video_frames: 33,
            video_steps: 20,
            video_cfg_scale: 6.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaJobStatus {
    pub job_id: Option<String>,
    /// idle | running | succeeded | failed | cancelled
    pub state: String,
    pub message: String,
    pub output_path: Option<PathBuf>,
    pub log_tail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEngineStatus {
    pub available: bool,
    pub sd_cli_path: PathBuf,
    pub message: String,
}

struct JobRecord {
    id: String,
    state: String,
    message: String,
    output_path: PathBuf,
    log_path: PathBuf,
}

struct Inner {
    settings: MediaSettings,
    settings_loaded: bool,
    job: Option<JobRecord>,
    child: Option<Child>,
}

/// 独立 Tauri managed state；任一时间最多运行一个媒体任务。
pub struct MediaManager {
    inner: Mutex<Inner>,
}

impl MediaManager {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                settings: MediaSettings::default(),
                settings_loaded: false,
                job: None,
                child: None,
            }),
        }
    }
}

impl Default for MediaManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MediaManager {
    fn drop(&mut self) {
        if let Ok(inner) = self.inner.get_mut() {
            if let Some(child) = inner.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn lock_inner(manager: &MediaManager) -> std::sync::MutexGuard<'_, Inner> {
    manager
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("media_settings.json"))
        .map_err(|e| format!("无法取得应用数据目录：{e}"))
}

fn output_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("media").join("output"))
        .map_err(|e| format!("无法取得应用数据目录：{e}"))
}

fn load_settings_file(app: &AppHandle) -> Result<MediaSettings, String> {
    let path = settings_path(app)?;
    match fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MediaSettings::default()),
        Err(error) => Err(format!("读取媒体设置失败：{error}")),
    }
}

fn ensure_settings_loaded(app: &AppHandle, inner: &mut Inner) -> Result<(), String> {
    if !inner.settings_loaded {
        inner.settings = load_settings_file(app)?;
        inner.settings_loaded = true;
    }
    Ok(())
}

fn persist_settings(app: &AppHandle, settings: &MediaSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    let parent = path
        .parent()
        .ok_or_else(|| "媒体设置文件路径无效".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("创建媒体设置目录失败：{e}"))?;
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("序列化媒体设置失败：{e}"))?;
    fs::write(path, json).map_err(|e| format!("保存媒体设置失败：{e}"))
}

fn file_exists(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn require_file(path: &Path, description: &str) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err(format!("请先选择{description}文件"));
    }
    if !file_exists(path) {
        return Err(format!("{description}文件不存在：{}", path.display()));
    }
    Ok(())
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    for (label, value) in [("宽度", width), ("高度", height)] {
        if !(256..=1024).contains(&value) || value % 16 != 0 {
            return Err(format!("{label}必须在 256–1024 之间且为 16 的倍数"));
        }
    }
    Ok(())
}

fn validate_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        Err("提示词不能为空".to_string())
    } else {
        Ok(())
    }
}

fn log_tail(path: &Path) -> String {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let len = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return String::new(),
    };
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn idle_status() -> MediaJobStatus {
    MediaJobStatus {
        job_id: None,
        state: "idle".to_string(),
        message: "当前没有媒体任务".to_string(),
        output_path: None,
        log_tail: String::new(),
    }
}

fn status_from_inner(inner: &Inner) -> MediaJobStatus {
    match inner.job.as_ref() {
        Some(job) => MediaJobStatus {
            job_id: Some(job.id.clone()),
            state: job.state.clone(),
            message: job.message.clone(),
            output_path: Some(job.output_path.clone()),
            log_tail: log_tail(&job.log_path),
        },
        None => idle_status(),
    }
}

/// 如任务已退出，则在轮询时更新最终状态。
fn refresh_job(inner: &mut Inner) -> MediaJobStatus {
    let is_running = inner
        .job
        .as_ref()
        .map(|job| job.state == "running")
        .unwrap_or(false);
    if is_running {
        let polled = match inner.child.as_mut() {
            Some(child) => Some(child.try_wait()),
            None => None,
        };
        match polled {
            Some(Ok(None)) => {}
            Some(Ok(Some(exit))) => {
                inner.child = None;
                if let Some(job) = inner.job.as_mut() {
                    finish_job(job, exit);
                }
            }
            Some(Err(error)) => {
                if let Some(child) = inner.child.as_mut() {
                    let _ = child.kill();
                }
                inner.child = None;
                if let Some(job) = inner.job.as_mut() {
                    job.state = "failed".to_string();
                    job.message = format!("查询生成进程状态失败：{error}");
                }
            }
            None => {
                if let Some(job) = inner.job.as_mut() {
                    job.state = "failed".to_string();
                    job.message = "生成任务进程丢失".to_string();
                }
            }
        }
    }
    status_from_inner(inner)
}

fn finish_job(job: &mut JobRecord, exit: ExitStatus) {
    let valid_output = job
        .output_path
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false);
    if exit.success() && valid_output {
        job.state = "succeeded".to_string();
        job.message = "媒体生成完成".to_string();
    } else if exit.success() {
        let extension = job
            .output_path
            .extension()
            .map(|value| value.to_string_lossy().to_uppercase())
            .unwrap_or_else(|| "输出".to_string());
        job.state = "failed".to_string();
        job.message = format!(
            "sd-cli 已退出，但没有生成有效的 {extension} 文件。请确认所选 sd-cli 支持 {extension} 输出（视频需要 WebM），并查看下方引擎日志。"
        );
    } else {
        job.state = "failed".to_string();
        job.message = format!(
            "媒体生成失败（进程退出状态：{exit}）。请查看下方引擎日志；若日志提示未知参数，请升级 sd-cli 或切换模型模式。"
        );
    }
}

fn create_job_files(
    dir: &Path,
    extension: &str,
) -> Result<(String, PathBuf, PathBuf, File), String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建媒体输出目录失败：{e}"))?;
    for _ in 0..16 {
        let sequence = JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!("media-{now}-{sequence}");
        let output = dir.join(format!("{id}.{extension}"));
        let log = dir.join(format!("{id}.log"));
        if output.exists() {
            continue;
        }
        match OpenOptions::new().write(true).create_new(true).open(&log) {
            Ok(file) => return Ok((id, output, log, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("创建媒体任务日志失败：{error}")),
        }
    }
    Err("无法分配唯一的媒体任务文件名，请稍后重试".to_string())
}

fn start_process(
    app: &AppHandle,
    manager: &MediaManager,
    extension: &str,
    args: Vec<OsString>,
) -> Result<MediaJobStatus, String> {
    let mut inner = lock_inner(manager);
    ensure_settings_loaded(app, &mut inner)?;
    refresh_job(&mut inner);
    if inner
        .job
        .as_ref()
        .map(|job| job.state == "running")
        .unwrap_or(false)
    {
        return Err("已有媒体任务正在运行，请等待完成或先取消".to_string());
    }
    require_file(&inner.settings.sd_cli_path, "sd-cli")?;
    if !is_executable(&inner.settings.sd_cli_path) {
        return Err(format!(
            "sd-cli 没有执行权限：{}。请在终端运行 chmod +x \"{}\" 后重试",
            inner.settings.sd_cli_path.display(),
            inner.settings.sd_cli_path.display()
        ));
    }

    let dir = output_dir(app)?;
    let (id, output_path, log_path, log_file) = create_job_files(&dir, extension)?;
    let stderr_file = log_file
        .try_clone()
        .map_err(|e| format!("打开任务日志失败：{e}"))?;
    let mut command = Command::new(&inner.settings.sd_cli_path);
    command
        .args(args)
        .arg("-o")
        .arg(&output_path)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file));
    let child = command
        .spawn()
        .map_err(|e| format!("启动 sd-cli 失败，请检查程序权限和运行环境：{e}"))?;

    inner.job = Some(JobRecord {
        id,
        state: "running".to_string(),
        message: "媒体生成中".to_string(),
        output_path,
        log_path,
    });
    inner.child = Some(child);
    Ok(status_from_inner(&inner))
}

/// 读取 media_settings.json（不存在时返回默认设置）。
#[tauri::command]
pub fn get_media_settings(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
) -> Result<MediaSettings, String> {
    let mut inner = lock_inner(&manager);
    ensure_settings_loaded(&app, &mut inner)?;
    Ok(inner.settings.clone())
}

/// 保存完整媒体设置到 app_data_dir()/media_settings.json。
#[tauri::command]
pub fn save_media_settings(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
    settings: MediaSettings,
) -> Result<MediaSettings, String> {
    persist_settings(&app, &settings)?;
    let mut inner = lock_inner(&manager);
    inner.settings = settings.clone();
    inner.settings_loaded = true;
    Ok(settings)
}

/// 选择并保存 sd-cli 可执行文件；取消选择时返回 None。
#[tauri::command]
pub fn pick_sd_cli(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
) -> Result<Option<PathBuf>, String> {
    let dialog = rfd::FileDialog::new().set_title("选择 stable-diffusion.cpp sd-cli");
    #[cfg(target_os = "windows")]
    let dialog = dialog.add_filter("Windows 程序", &["exe"]);
    let selected = dialog.pick_file();
    if let Some(path) = selected.as_ref() {
        if !path.is_file() {
            return Err("所选 sd-cli 不是有效文件".to_string());
        }
        if !is_executable(path) {
            return Err(format!(
                "所选 sd-cli 没有执行权限：{}。请先运行 chmod +x 再选择",
                path.display()
            ));
        }
        let mut inner = lock_inner(&manager);
        ensure_settings_loaded(&app, &mut inner)?;
        inner.settings.sd_cli_path = path.clone();
        persist_settings(&app, &inner.settings)?;
    }
    Ok(selected)
}

/// 选择模型或输入图片，并更新对应设置字段。kind 支持 image_model/video_diffusion/video_vae/video_t5xxl/input_image。
#[tauri::command]
pub fn pick_media_file(
    kind: String,
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
) -> Result<Option<PathBuf>, String> {
    let (title, filters): (&str, &[&str]) = match kind.as_str() {
        "image_model" => ("选择图片模型（单文件 checkpoint 或 Z-Image diffusion）", &["gguf", "safetensors", "ckpt"]),
        "image_vae" => ("选择图片 VAE（Z-Image / Flux 分离式模型）", &["safetensors", "gguf"]),
        "image_text_encoder" => ("选择图片 LLM 文本编码器（Z-Image 的 Qwen3-4B）", &["safetensors", "gguf"]),
        "video_diffusion" => ("选择 Wan 视频扩散模型（Wan2.2 A14B 选 LowNoise）", &["gguf", "safetensors"]),
        "video_vae" => ("选择视频 VAE", &["gguf", "safetensors"]),
        "video_t5xxl" => ("选择 T5XXL / UMT5 文本编码器", &["gguf", "safetensors"]),
        "video_clip_vision" => ("选择 CLIP Vision（Wan2.1 I2V）", &["safetensors", "gguf"]),
        "video_high_noise_diffusion" => ("选择 Wan2.2 A14B HighNoise 扩散模型", &["gguf", "safetensors"]),
        "input_image" => ("选择视频输入图片（可选）", &["png", "jpg", "jpeg", "webp"]),
        _ => return Err("不支持的文件类型，请使用 image_model、image_vae、image_text_encoder、video_diffusion、video_vae、video_t5xxl、video_clip_vision、video_high_noise_diffusion 或 input_image".to_string()),
    };
    let selected = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("支持的文件", filters)
        .pick_file();
    if let Some(path) = selected.as_ref() {
        if !path.is_file() {
            return Err("所选路径不是有效文件".to_string());
        }
        let mut inner = lock_inner(&manager);
        ensure_settings_loaded(&app, &mut inner)?;
        match kind.as_str() {
            "image_model" => inner.settings.image_model = path.clone(),
            "image_vae" => inner.settings.image_vae = path.clone(),
            "image_text_encoder" => inner.settings.image_text_encoder = path.clone(),
            "video_diffusion" => inner.settings.video_diffusion = path.clone(),
            "video_vae" => inner.settings.video_vae = path.clone(),
            "video_t5xxl" => inner.settings.video_t5xxl = path.clone(),
            "video_clip_vision" => inner.settings.video_clip_vision = path.clone(),
            "video_high_noise_diffusion" => {
                inner.settings.video_high_noise_diffusion = path.clone()
            }
            "input_image" => inner.settings.input_image = Some(path.clone()),
            _ => return Err("不支持的文件类型".to_string()),
        }
        persist_settings(&app, &inner.settings)?;
    }
    Ok(selected)
}

/// 检查 sd-cli 当前配置；不联网、不下载或探测远端服务。
#[tauri::command]
pub fn get_media_engine_status(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
) -> Result<MediaEngineStatus, String> {
    let mut inner = lock_inner(&manager);
    ensure_settings_loaded(&app, &mut inner)?;
    let path = &inner.settings.sd_cli_path;
    let available = file_exists(path) && is_executable(path);
    Ok(MediaEngineStatus {
        available,
        sd_cli_path: path.clone(),
        message: if available {
            "本地 sd-cli 已就绪".to_string()
        } else if file_exists(path) {
            "已选择 sd-cli，但它没有执行权限；请运行 chmod +x 后重新选择".to_string()
        } else {
            "尚未配置有效的 sd-cli，请选择本地可执行文件".to_string()
        },
    })
}

/// 启动图片生成。宽高必须为 256–1024 的 16 倍数；输出始终位于应用数据目录。
#[tauri::command]
pub fn generate_image(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
    prompt: String,
    negative_prompt: Option<String>,
    width: u32,
    height: u32,
    steps: u32,
    cfg_scale: f32,
) -> Result<MediaJobStatus, String> {
    validate_prompt(&prompt)?;
    validate_dimensions(width, height)?;
    if !(1..=150).contains(&steps) {
        return Err("图片生成步数必须在 1–150 之间".to_string());
    }
    if !cfg_scale.is_finite() || !(0.0..=30.0).contains(&cfg_scale) {
        return Err("CFG Scale 必须是 0–30 之间的有限数值".to_string());
    }
    let settings = {
        let mut inner = lock_inner(&manager);
        ensure_settings_loaded(&app, &mut inner)?;
        require_file(&inner.settings.image_model, "图片模型")?;
        inner.settings.clone()
    };
    let separate_components = !settings.image_vae.as_os_str().is_empty()
        || !settings.image_text_encoder.as_os_str().is_empty();
    let mut args = vec![OsString::from("-M"), OsString::from("img_gen")];
    if separate_components {
        // Z-Image / Flux 等分离式模型：diffusion + VAE + LLM/文本编码器。
        require_file(&settings.image_vae, "图片 VAE")?;
        require_file(&settings.image_text_encoder, "图片 LLM 文本编码器")?;
        args.extend([
            OsString::from("--diffusion-model"),
            settings.image_model.into_os_string(),
            OsString::from("--vae"),
            settings.image_vae.into_os_string(),
            OsString::from("--llm"),
            settings.image_text_encoder.into_os_string(),
        ]);
    } else {
        args.extend([OsString::from("-m"), settings.image_model.into_os_string()]);
    }
    args.extend([
        OsString::from("-p"),
        prompt.into(),
        OsString::from("-n"),
        negative_prompt.unwrap_or_default().into(),
        OsString::from("-W"),
        width.to_string().into(),
        OsString::from("-H"),
        height.to_string().into(),
        OsString::from("--steps"),
        steps.to_string().into(),
        OsString::from("--cfg-scale"),
        cfg_scale.to_string().into(),
    ]);
    start_process(&app, &manager, "png", args)
}

/// 校验视频模型组合，避免 UI 允许选择、运行时才因缺参数失败。
fn validate_video_combination(
    kind: &str,
    has_image: bool,
    clip_vision: &Path,
    high_noise: &Path,
) -> Result<(), String> {
    match kind {
        "wan21_t2v" => {
            if has_image {
                return Err(
                    "Wan2.1 T2V 模式不支持首帧图。请清除首帧，或切换到 Wan2.1 I2V 模式"
                        .to_string(),
                );
            }
        }
        "wan21_i2v" => {
            if !has_image {
                return Err("Wan2.1 I2V 模式需要选择首帧图片".to_string());
            }
            require_file(clip_vision, "CLIP Vision")?;
        }
        "wan22_ti2v" => {}
        "wan22_a14b" => {
            require_file(high_noise, "Wan2.2 A14B high-noise 扩散模型")?;
        }
        _ => {
            return Err(
                "视频模型模式无效；请选择 Wan2.1 T2V、Wan2.1 I2V、Wan2.2 TI2V 5B 或 Wan2.2 A14B"
                    .to_string(),
            )
        }
    }
    Ok(())
}

fn build_video_args(
    settings: &MediaSettings,
    prompt: &str,
    width: u32,
    height: u32,
    frames: u32,
    input_image: Option<PathBuf>,
) -> Result<Vec<OsString>, String> {
    require_file(&settings.video_diffusion, "视频扩散模型")?;
    require_file(&settings.video_vae, "视频 VAE")?;
    require_file(&settings.video_t5xxl, "T5XXL / UMT5 文本编码器")?;
    let t5_name = settings
        .video_t5xxl
        .file_name()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if t5_name.contains("fp8") || t5_name.contains("_f8") {
        return Err(
            "当前 M1/Metal 的 sd-cli 缺少该 FP8 T5 的 Metal kernel。请先把 T5 转为 GGUF Q8（或 F16），再在视频页选择转换后的文件。"
                .to_string(),
        );
    }
    if !(1..=150).contains(&settings.video_steps) {
        return Err("视频步数必须在 1–150 之间".to_string());
    }
    if !settings.video_cfg_scale.is_finite() || !(0.0..=30.0).contains(&settings.video_cfg_scale) {
        return Err("视频 CFG Scale 必须是 0–30 之间的有限数值".to_string());
    }
    if let Some(path) = input_image.as_ref() {
        require_file(path, "输入图片")?;
    }
    validate_video_combination(
        &settings.video_model_kind,
        input_image.is_some(),
        &settings.video_clip_vision,
        &settings.video_high_noise_diffusion,
    )?;

    let mut args = vec![
        OsString::from("-M"),
        OsString::from("vid_gen"),
        OsString::from("--diffusion-model"),
        settings.video_diffusion.clone().into_os_string(),
    ];
    if settings.video_model_kind == "wan22_a14b" {
        args.push(OsString::from("--high-noise-diffusion-model"));
        args.push(
            settings
                .video_high_noise_diffusion
                .clone()
                .into_os_string(),
        );
    }
    args.extend([
        OsString::from("--vae"),
        settings.video_vae.clone().into_os_string(),
        OsString::from("--t5xxl"),
        settings.video_t5xxl.clone().into_os_string(),
    ]);
    if settings.video_model_kind == "wan21_i2v" {
        args.push(OsString::from("--clip_vision"));
        args.push(settings.video_clip_vision.clone().into_os_string());
    }
    // Wan2.2 TI2V 5B 的 3D VAE 在当前 Apple Silicon Metal 上会卡在
    // ggml_metal_synchronize；把 VAE 放到 CPU 可稳定完成（较慢）。
    if settings.video_model_kind == "wan22_ti2v" {
        args.extend([OsString::from("--backend"), OsString::from("vae=cpu")]);
    }
    args.extend([
        OsString::from("-p"),
        OsString::from(prompt),
        OsString::from("-W"),
        OsString::from(width.to_string()),
        OsString::from("-H"),
        OsString::from(height.to_string()),
        OsString::from("--video-frames"),
        OsString::from(frames.to_string()),
        OsString::from("--steps"),
        OsString::from(settings.video_steps.to_string()),
        OsString::from("--cfg-scale"),
        OsString::from(settings.video_cfg_scale.to_string()),
    ]);
    if let Some(path) = input_image {
        args.push(OsString::from("-i"));
        args.push(path.into_os_string());
    }
    Ok(args)
}

/// 启动 Wan 视频生成。frames 仅允许 33/77/81，可选输入图像来自参数或已保存设置。
#[tauri::command]
pub fn generate_video(
    app: AppHandle,
    manager: tauri::State<'_, MediaManager>,
    prompt: String,
    width: u32,
    height: u32,
    frames: u32,
    input_image: Option<PathBuf>,
) -> Result<MediaJobStatus, String> {
    validate_prompt(&prompt)?;
    validate_dimensions(width, height)?;
    if ![33, 77, 81].contains(&frames) {
        return Err("视频帧数仅支持 33、77 或 81".to_string());
    }
    let settings = {
        let mut inner = lock_inner(&manager);
        ensure_settings_loaded(&app, &mut inner)?;
        inner.settings.clone()
    };
    let selected_image = input_image.or(settings.input_image.clone());
    let args = build_video_args(&settings, &prompt, width, height, frames, selected_image)?;
    start_process(&app, &manager, "webm", args)
}

/// 查询当前任务；进程退出状态在本次轮询时刷新，日志仅返回末尾 12 KiB。
#[tauri::command]
pub fn get_media_job_status(manager: tauri::State<'_, MediaManager>) -> MediaJobStatus {
    refresh_job(&mut lock_inner(&manager))
}

/// 在系统文件管理器中定位输出文件；仅允许访问 LocalRun 的媒体输出目录。
#[tauri::command]
pub fn reveal_media_output(app: AppHandle, path: String) -> Result<(), String> {
    let root = output_dir(&app)?;
    fs::create_dir_all(&root).map_err(|e| format!("创建媒体输出目录失败：{e}"))?;
    let root = fs::canonicalize(&root).map_err(|e| format!("解析媒体输出目录失败：{e}"))?;
    let target = fs::canonicalize(PathBuf::from(path))
        .map_err(|e| format!("生成文件不存在或无法访问：{e}"))?;
    if !target.is_file() || !target.starts_with(&root) {
        return Err("只能定位 LocalRun 媒体输出目录中的文件".to_string());
    }

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg("-R").arg(&target);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(format!("/select,{}", target.display()));
        command
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target.parent().unwrap_or(&root));
        command
    };
    command
        .spawn()
        .map_err(|e| format!("无法打开系统文件管理器：{e}"))?;
    Ok(())
}

/// 取消当前进程。任务已经结束时原样返回最终状态。
#[tauri::command]
pub fn cancel_media_job(
    manager: tauri::State<'_, MediaManager>,
    job_id: String,
) -> Result<MediaJobStatus, String> {
    let mut inner = lock_inner(&manager);
    let current = refresh_job(&mut inner);
    if current.job_id.as_deref() != Some(job_id.as_str()) {
        return Err("找不到对应的媒体任务".to_string());
    }
    if current.state != "running" {
        return Ok(current);
    }
    let child = inner
        .child
        .as_mut()
        .ok_or_else(|| "媒体任务进程不存在，无法取消".to_string())?;
    if let Err(error) = child.kill() {
        // kill 可能与进程自然退出同时发生；尽量把实际退出状态刷新出来。
        let polled = child.try_wait();
        if let Ok(Some(exit)) = polled {
            inner.child = None;
            if let Some(job) = inner.job.as_mut() {
                finish_job(job, exit);
            }
            return Ok(status_from_inner(&inner));
        }
        return Err(format!("取消媒体任务失败：{error}"));
    }
    if let Some(child) = inner.child.as_mut() {
        let _ = child.wait();
    }
    inner.child = None;
    if let Some(job) = inner.job.as_mut() {
        job.state = "cancelled".to_string();
        job.message = "媒体任务已取消".to_string();
    }
    Ok(status_from_inner(&inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "localrun-media-test-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn touch(path: &Path) {
        fs::write(path, b"test").unwrap();
    }

    fn base_settings(root: &Path) -> MediaSettings {
        let mut settings = MediaSettings::default();
        settings.video_diffusion = root.join("wan-diffusion.gguf");
        settings.video_vae = root.join("wan-vae.safetensors");
        settings.video_t5xxl = root.join("umt5.gguf");
        touch(&settings.video_diffusion);
        touch(&settings.video_vae);
        touch(&settings.video_t5xxl);
        settings
    }

    fn rendered(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn has_pair(args: &[String], flag: &str, value: &Path) -> bool {
        let value = value.display().to_string();
        args.windows(2).any(|pair| pair[0] == flag && pair[1] == value)
    }

    #[test]
    fn wan21_i2v_requires_and_passes_clip_vision() {
        let root = temp_root("wan21-i2v");
        let mut settings = base_settings(&root);
        settings.video_model_kind = "wan21_i2v".to_string();
        let image = root.join("first.png");
        touch(&image);

        let error = build_video_args(&settings, "cat", 480, 832, 33, Some(image.clone()))
            .unwrap_err();
        assert!(error.contains("CLIP Vision"), "{error}");

        settings.video_clip_vision = root.join("clip_vision_h.safetensors");
        touch(&settings.video_clip_vision);
        let args = build_video_args(&settings, "cat", 480, 832, 33, Some(image)).unwrap();
        let args = rendered(&args);
        assert!(has_pair(&args, "--clip_vision", &settings.video_clip_vision));
        assert!(has_pair(&args, "-i", &root.join("first.png")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wan21_t2v_rejects_input_image() {
        let root = temp_root("wan21-t2v");
        let settings = base_settings(&root);
        let image = root.join("first.png");
        touch(&image);
        let error = build_video_args(&settings, "cat", 832, 480, 33, Some(image)).unwrap_err();
        assert!(error.contains("不支持首帧图"), "{error}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wan22_a14b_requires_and_passes_high_noise_model() {
        let root = temp_root("wan22-a14b");
        let mut settings = base_settings(&root);
        settings.video_model_kind = "wan22_a14b".to_string();

        let error = build_video_args(&settings, "cat", 832, 480, 33, None).unwrap_err();
        assert!(error.contains("high-noise"), "{error}");

        settings.video_high_noise_diffusion = root.join("high-noise.gguf");
        touch(&settings.video_high_noise_diffusion);
        let args = build_video_args(&settings, "cat", 832, 480, 33, None).unwrap();
        let args = rendered(&args);
        assert!(has_pair(
            &args,
            "--high-noise-diffusion-model",
            &settings.video_high_noise_diffusion
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wan22_ti2v_does_not_require_clip_vision_or_high_noise() {
        let root = temp_root("wan22-ti2v");
        let mut settings = base_settings(&root);
        settings.video_model_kind = "wan22_ti2v".to_string();
        let image = root.join("first.png");
        touch(&image);
        let args = build_video_args(&settings, "cat", 480, 832, 33, Some(image)).unwrap();
        let args = rendered(&args);
        assert!(!args.iter().any(|arg| arg == "--clip_vision"));
        assert!(!args.iter().any(|arg| arg == "--high-noise-diffusion-model"));
        fs::remove_dir_all(&root).unwrap();
    }
}
