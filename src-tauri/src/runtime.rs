//! 推理运行时：扫描本地 GGUF 模型 → 启动/停止 llama-server 子进程 → 健康轮询。
//! 模型加载完全本地，不走任何云服务；端口默认 8686（设置可改）。

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;

#[derive(Serialize, Clone, Debug)]
pub struct LocalModel {
    pub path: String,
    pub name: String,
    pub size_mb: u64,
    /// GGUF 元数据：Transformer 层数（KV 缓存估算用）
    pub n_layers: u32,
    /// GGUF 元数据：隐藏维度（KV 缓存估算用）
    pub n_embd: u32,
    /// GGUF 元数据：模型原生上下文长度
    pub context_len: u32,
}

#[derive(Serialize, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub model: String,
    pub pid: Option<u32>,
    pub ready: bool,
    pub port: u16,
    pub message: String,
    pub log: Vec<String>,
}

pub struct RuntimeManager {
    pub status: Mutex<RuntimeStatus>,
    pub child: Mutex<Option<Child>>,
}

impl RuntimeManager {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(RuntimeStatus {
                running: false,
                model: String::new(),
                pid: None,
                ready: false,
                port: 8686,
                message: "未启动".to_string(),
                log: Vec::new(),
            }),
            child: Mutex::new(None),
        }
    }
}

impl Drop for RuntimeManager {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            if let Some(mut child) = child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn run_log_path() -> PathBuf {
    crate::paths::app_data_dir().join("engine").join("run.log")
}

fn debug_log(line: &str) {
    let p = crate::paths::app_data_dir()
        .join("engine")
        .join("debug.log");
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(p) {
        use std::io::Write;
        let _ = writeln!(f, "{}", line);
    }
}

fn read_tail_log(max_lines: usize) -> Vec<String> {
    let p = run_log_path();
    let content = match fs::read_to_string(&p) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

fn append_log(log: &mut Vec<String>, line: &str) {
    log.push(line.to_string());
    if log.len() > 60 {
        log.remove(0);
    }
}

/// 读取 GGUF 头部元数据：返回 (block_count, embedding_length, context_length)。
/// 架构前缀从 general.architecture 读取（qwen2/llama/mistral…），按 {arch}.xxx 匹配。
/// 只解析文件前 128KB 的标量 KV，遇到 string/array 等复杂值按规则跳过/终止。
fn read_gguf_meta(path: &str) -> (u32, u32, u32) {
    use std::io::Read;
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (0, 0, 0),
    };
    let mut buf = [0u8; 131072];
    let n = f.read(&mut buf).unwrap_or(0);
    if n < 24 || &buf[0..4] != b"GGUF" {
        return (0, 0, 0);
    }
    let kv_count = u64::from_le_bytes(buf[16..24].try_into().unwrap_or([0u8; 8]));
    let mut pos = 24usize;
    let mut layers = 0u32;
    let mut embd = 0u32;
    let mut ctx_native = 0u32;
    let mut arch = String::new();
    for _ in 0..kv_count.min(40000) {
        if pos + 8 > n {
            break;
        }
        let klen = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap_or([0u8; 8])) as usize;
        pos += 8;
        if pos + klen > n {
            break;
        }
        let key = String::from_utf8_lossy(&buf[pos..pos + klen]).to_string();
        pos += klen;
        if pos + 4 > n {
            break;
        }
        let vtype = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap_or([0u8; 4]));
        pos += 4;
        let mut val: u32 = 0;
        match vtype {
            // uint8 / int8 / bool
            0 | 1 | 7 => {
                if pos + 1 > n {
                    break;
                }
                val = buf[pos] as u32;
                pos += 1;
            }
            // uint16 / int16（GGUF 类型 2/3，必须读 2 字节，不能按 1 字节处理）
            2 | 3 => {
                if pos + 2 > n {
                    break;
                }
                val = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap_or([0u8; 2])) as u32;
                pos += 2;
            }
            // u32 / int32 / float32
            4 | 5 | 6 => {
                if pos + 4 > n {
                    break;
                }
                val = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap_or([0u8; 4]));
                pos += 4;
            }
            // u64 / int64 / float64：取低 32 位
            10 | 11 | 12 => {
                if pos + 8 > n {
                    break;
                }
                val = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap_or([0u8; 8])) as u32;
                pos += 8;
            }
            // string
            8 => {
                if pos + 8 > n {
                    break;
                }
                let slen =
                    u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap_or([0u8; 8])) as usize;
                pos += 8;
                if pos + slen > n {
                    break;
                }
                if key == "general.architecture" {
                    arch = String::from_utf8_lossy(&buf[pos..pos + slen]).to_string();
                }
                pos += slen;
            }
            // array（tokenizer 等大块）：已拿到的数据足够则返回，否则终止
            _ => break,
        }
        if vtype != 8 && !arch.is_empty() {
            let bc = format!("{arch}.block_count");
            let el = format!("{arch}.embedding_length");
            let cl = format!("{arch}.context_length");
            if key == bc {
                layers = val;
            } else if key == el {
                embd = val;
            } else if key == cl {
                ctx_native = val;
            }
        }
        if layers > 0 && embd > 0 && ctx_native > 0 {
            break;
        }
    }
    (layers, embd, ctx_native)
}

/// 获取本机局域网 IPv4 地址（lan_access 开启时前端展示 API 地址用）
#[tauri::command]
pub fn get_local_ip() -> Vec<String> {
    get_local_ip_impl()
}

#[cfg(target_os = "windows")]
fn get_local_ip_impl() -> Vec<String> {
    let mut out = Vec::new();
    let out_bin = match std::process::Command::new("ipconfig").output() {
        Ok(o) => o,
        Err(_) => return out,
    };
    let text = String::from_utf8_lossy(&out_bin.stdout).to_string();
    for line in text.lines() {
        let t = line.trim();
        let v4 = t
            .find("IPv4 Address")
            .or_else(|| t.find("IPv4 地址"))
            .or_else(|| t.find("IPv4地址"));
        if let Some(idx) = v4 {
            let rest = &t[idx..];
            if let Some(c) = rest.find(':').or_else(|| rest.find('：')) {
                let ip = rest[c + 1..].trim().to_string();
                if ip.split('.').count() == 4 && !ip.is_empty() {
                    out.push(ip);
                }
            }
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn get_local_ip_impl() -> Vec<String> {
    // macOS/Linux：ifconfig 输出 "inet 192.168.x.x"
    let mut out = Vec::new();
    let out_bin = match std::process::Command::new("ifconfig").output() {
        Ok(o) => o,
        Err(_) => return out,
    };
    let text = String::from_utf8_lossy(&out_bin.stdout).to_string();
    for line in text.lines() {
        let t = line.trim();
        let rest = t.strip_prefix("inet ").or_else(|| t.strip_prefix("inet\t"));
        if let Some(r) = rest {
            let ip = r.split_whitespace().next().unwrap_or("").trim().to_string();
            if ip.split('.').count() == 4 && !ip.is_empty() {
                out.push(ip);
            }
        }
    }
    out
}

/// 弹出原生文件对话框选择 .gguf（返回绝对路径；取消返回 None）
#[tauri::command]
pub async fn pick_gguf() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let f = rfd::FileDialog::new()
            .set_title("选择要导入的 GGUF 模型")
            .add_filter("GGUF 模型", &["gguf"])
            .pick_file();
        Ok(f.map(|p| p.display().to_string()))
    })
    .await
    .map_err(|e| format!("文件对话框线程错误：{e}"))?
}

/// 手动导入本地 GGUF：校验扩展名 + 持久化路径（不移动文件），返回模型信息
#[tauri::command]
pub fn import_local_model(
    app: tauri::AppHandle,
    state: State<'_, crate::AppState>,
    path: String,
) -> Result<LocalModel, String> {
    let p = path;
    if !p.to_lowercase().ends_with(".gguf") {
        return Err("只能导入 .gguf 格式的模型文件".to_string());
    }
    let meta = fs::metadata(&p).map_err(|e| format!("无法访问 {p}：{e}"))?;
    if !meta.is_file() {
        return Err("目标不是文件".to_string());
    }
    let mut settings = state.settings.lock().unwrap().clone();
    if !settings
        .extra_models
        .iter()
        .any(|x| x.eq_ignore_ascii_case(&p))
    {
        settings.extra_models.push(p.clone());
        crate::settings::save(&app, &settings).map_err(|e| format!("保存设置失败：{e}"))?;
        *state.settings.lock().unwrap() = settings;
    }
    let size = meta.len();
    let name = PathBuf::from(&p)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.clone());
    let (l, e, c) = read_gguf_meta(&p);
    Ok(LocalModel {
        path: p,
        name,
        size_mb: size / (1024 * 1024),
        n_layers: l,
        n_embd: e,
        context_len: c,
    })
}

/// 递归扫描模型目录下的 .gguf 文件
#[tauri::command]
pub fn list_local_models(state: State<'_, crate::AppState>) -> Vec<LocalModel> {
    let settings = state.settings.lock().unwrap();
    let dir = settings.models_dir.clone();
    let extra = settings.extra_models.clone();
    drop(settings);
    let mut out = Vec::new();
    if fs::read_dir(&dir).is_ok() {
        let mut stack: Vec<PathBuf> = vec![PathBuf::from(&dir)];
        while let Some(d) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&d) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().map(|x| x == "gguf").unwrap_or(false) {
                        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                        let name = p
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| p.display().to_string());
                        out.push(LocalModel {
                            path: p.display().to_string(),
                            name,
                            size_mb: size / (1024 * 1024),
                            n_layers: 0,
                            n_embd: 0,
                            context_len: 0,
                        });
                        // 解析 GGUF 元数据（层数/隐藏维度/原生上下文），用于 KV 缓存内存预估
                        let (l, e, c) = read_gguf_meta(&p.display().to_string());
                        if let Some(m) = out.last_mut() {
                            m.n_layers = l;
                            m.n_embd = e;
                            m.context_len = c;
                        }
                    }
                }
            }
        }
    }
    // 合并手动导入的模型（去重）
    for p in extra {
        if fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false)
            && !out.iter().any(|m| m.path.eq_ignore_ascii_case(&p))
        {
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let name = PathBuf::from(&p)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone());
            let (l, e, c) = read_gguf_meta(&p);
            out.push(LocalModel {
                path: p,
                name,
                size_mb: size / (1024 * 1024),
                n_layers: l,
                n_embd: e,
                context_len: c,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn check_health(port: u16) -> bool {
    let client = match crate::net::build_local_client() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let url = format!("http://127.0.0.1:{port}/health");
    match client.get(&url).send().await {
        Ok(r) => {
            if !r.status().is_success() {
                return false;
            }
            if let Ok(b) = r.text().await {
                return b.contains("\"status\":\"ok\"") || b.contains("ok");
            }
            true
        }
        Err(_) => false,
    }
}

/// 启动本地模型：拼参数 spawn llama-server 并轮询 /health 直至就绪。
/// 返回 ()；前端随后用 get_runtime_status 拉取最新状态（规避返回值序列化链路问题）。
#[tauri::command]
pub async fn start_model(state: State<'_, crate::AppState>, path: String) -> Result<(), String> {
    debug_log(&format!("start_model called: {path}"));
    // 已有进程先停（短锁）
    {
        let mut child_guard = state.runtime.child.lock().unwrap();
        if let Some(mut old) = child_guard.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
    }
    debug_log("start_model: old process handled");
    // 取端口、上下文长度、局域网开关、思考模式（短锁）
    let (port, ctx_len, lan_access, reasoning);
    {
        let st = state.settings.lock().unwrap();
        port = st.port;
        ctx_len = st.ctx_len;
        lan_access = st.lan_access;
        reasoning = st.reasoning;
    }
    // 预置启动中状态（短锁）
    {
        let mut st = state.runtime.status.lock().unwrap();
        *st = RuntimeStatus {
            running: true,
            model: path.clone(),
            pid: None,
            ready: false,
            port,
            message: "启动中…".to_string(),
            log: Vec::new(),
        };
    }
    debug_log("start_model: status preset, port = {port}");

    let exe = crate::engine::load_status().exe;
    if exe.is_empty() || !Path::new(&exe).exists() {
        {
            let mut st = state.runtime.status.lock().unwrap();
            *st = RuntimeStatus {
                running: false,
                model: path,
                pid: None,
                ready: false,
                port,
                message: "推理引擎未安装，请先在运行页一键安装".to_string(),
                log: Vec::new(),
            };
        }
        return Err("推理引擎未安装".to_string());
    }

    // GPU 分层规划：旧逻辑在大模型略大于“可用显存”时直接 -ngl 0（全 CPU），
    // 在 Apple Silicon 上尤其慢。这里改为按总统一内存/显存预算做部分或全量 offload。
    let model_mb = fs::metadata(&path)
        .map(|m| m.len() / (1024 * 1024))
        .unwrap_or(0);
    let hw = crate::hardware::get_hardware_info();
    let budget_mb = if hw.gpu.unified_memory {
        // 统一内存需要给系统和 KV 缓存留空间；用总内存的 3/4 再留 2 GB。
        (hw.ram_total_gb * 1024 * 3 / 4).saturating_sub(2048)
    } else {
        hw.gpu.vram_total_mb.saturating_sub(1024)
    };
    let ngl = if model_mb == 0 || budget_mb == 0 {
        "0".to_string()
    } else if model_mb <= budget_mb {
        "999".to_string()
    } else {
        let (layers, _, _) = read_gguf_meta(&path);
        if layers == 0 {
            "0".to_string()
        } else {
            let partial = ((budget_mb as f64 / model_mb as f64) * layers as f64).floor() as u32;
            partial.max(1).min(layers).to_string()
        }
    };

    // 日志重定向到 run.log
    fs::create_dir_all(run_log_path().parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("创建日志目录失败: {e}"))?;
    let logf = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(run_log_path())
        .map_err(|e| format!("打开日志文件失败: {e}"))?;
    let errf = logf
        .try_clone()
        .map_err(|e| format!("克隆日志句柄失败: {e}"))?;

    let model_name = Path::new(&path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut cmd = Command::new(&exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：隐藏 llama-server 的控制台黑窗口
        cmd.creation_flags(0x08000000);
    }
    cmd.args([
        "--model",
        &path,
        "--host",
        if lan_access { "0.0.0.0" } else { "127.0.0.1" },
        "--port",
        &port.to_string(),
        "-c",
        &ctx_len.to_string(),
        "-ngl",
        &ngl,
    ]);
    if reasoning {
        // 仅对支持 reasoning 的模型生效（如 DeepSeek-R1 系列）；普通模型忽略该参数
        cmd.arg("--reasoning").arg("on");
    }
    cmd.arg("--no-webui");
    // llama-server 必须启用 Jinja 聊天模板，否则 tools/tool_calls 不会按模型模板编码。
    cmd.arg("--jinja");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(logf))
        .stderr(Stdio::from(errf));

    let child = cmd.spawn().map_err(|e| {
        debug_log(&format!("start_model: spawn failed: {e}"));
        {
            let mut st = state.runtime.status.lock().unwrap();
            *st = RuntimeStatus {
                running: false,
                model: path.clone(),
                pid: None,
                ready: false,
                port,
                message: format!("启动失败：{e}"),
                log: read_tail_log(20),
            };
        }
        format!("启动 llama-server 失败：{e}")
    })?;
    let pid = child.id();
    debug_log(&format!("start_model: spawned pid {pid}"));
    {
        let mut cg = state.runtime.child.lock().unwrap();
        *cg = Some(child);
    }
    {
        // 先释放 child 锁再取 status，避免与 stop_model 的 status→child 锁顺序形成死锁。
        let mut st = state.runtime.status.lock().unwrap();
        st.pid = Some(pid);
        append_log(&mut st.log, &format!("启动 llama-server：{model_name}"));
        append_log(
            &mut st.log,
            &format!(
                "参数：--port {port} -c {ctx_len} -ngl {ngl} --jinja host {}",
                if lan_access { "0.0.0.0" } else { "127.0.0.1" }
            ),
        );
        append_log(&mut st.log, &format!("进程 PID {pid}"));
    }

    // 轮询 /health（最多 300s）；同时检测 llama-server 是否提前退出，
    // 例如 macOS 上缺 dylib 时会立刻退出，不能只报“超时”。
    let mut ready = false;
    let mut exited: Option<std::process::ExitStatus> = None;
    for i in 0..600 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if check_health(port).await {
            ready = true;
            debug_log(&format!("start_model: health ok at loop {i}"));
            break;
        }
        let status = {
            let mut child_guard = state.runtime.child.lock().unwrap();
            let status = child_guard
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten());
            if status.is_some() {
                *child_guard = None;
            }
            status
        };
        if let Some(status) = status {
            debug_log(&format!("start_model: server exited early: {status}"));
            exited = Some(status);
            break;
        }
    }
    debug_log(&format!(
        "start_model: polling done, ready={ready}, exited={exited:?}"
    ));
    {
        debug_log("start_model: final status update");
        let mut st = state.runtime.status.lock().unwrap();
        debug_log("final: status lock ok");
        st.ready = ready;
        if ready {
            st.message = "运行中（本地 API 已就绪）".to_string();
        } else if let Some(status) = exited {
            st.running = false;
            st.pid = None;
            st.message = format!("llama-server 启动后退出（{status}），请查看日志");
            st.log = read_tail_log(40);
            append_log(&mut st.log, "进程在健康检查通过前退出");
        } else {
            st.message = "启动超时（300 秒），进程仍在运行但 API 尚未就绪；可停止后重试".to_string();
            st.log = read_tail_log(40);
            append_log(&mut st.log, "健康检查未通过");
        }
        debug_log("final: block end");
    }
    debug_log("start_model: returning Ok(())");
    Ok(())
}

/// 停止当前推理进程
#[tauri::command]
pub fn stop_model(state: State<'_, crate::AppState>) -> RuntimeStatus {
    let mut st = state.runtime.status.lock().unwrap();
    if let Some(mut child) = state.runtime.child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *st = RuntimeStatus {
        running: false,
        model: String::new(),
        pid: None,
        ready: false,
        port: st.port,
        message: "已停止".to_string(),
        log: st.log.clone(),
    };
    st.clone()
}

/// 当前运行时状态（前端轮询用）
#[tauri::command]
pub fn get_runtime_status(state: State<'_, crate::AppState>) -> RuntimeStatus {
    debug_log("get_runtime_status called");
    let st = state.runtime.status.lock().unwrap().clone();
    debug_log("get_runtime_status: lock ok");
    st
}
