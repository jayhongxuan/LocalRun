//! M3-3 工具执行引擎：模型发起 tool_calls 后，前端调 execute_tool 执行，
//! 结果以 role=tool 消息回填给模型，模型据此给出最终回答。
//! 当前实现：run_command（Windows PowerShell/macOS 与 Linux shell）、read_file（读文本）、web_search（走 HF 搜索）。

use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

/// 单次命令的硬超时与输出上限。输出最终还会被 `clip` 截到 6000 字符。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;

/// 拖拽附件：路径 → 名称/大小/类型 + 图片 base64（≤15MB）或文本内容（≤12000 字符）
#[derive(serde::Serialize, Clone)]
pub struct Attachment {
    pub name: String,
    pub size: u64,
    pub kind: String, // image | text | file
    pub data_url: String,
    pub text: String,
}

/// 工具返回给前端的图片缩略图；只用于 UI 展示，不会作为 base64 回填给模型。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ToolImage {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub data_url: String,
}

/// 工具执行结果：content 回填给模型，images 由前端渲染给用户。
#[derive(serde::Serialize)]
pub struct ToolResult {
    pub content: String,
    pub images: Vec<ToolImage>,
}

impl ToolResult {
    fn text(content: String) -> Self {
        Self {
            content,
            images: Vec::new(),
        }
    }
}

#[tauri::command]
pub fn read_attachment(path: String) -> Result<Attachment, String> {
    let p = path;
    let meta = std::fs::metadata(&p).map_err(|e| format!("无法访问 {p}：{e}"))?;
    let size = meta.len();
    let name = std::path::Path::new(&p)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.clone());
    let lower = name.to_lowercase();
    let is_img = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "svg"]
        .iter()
        .any(|e| lower.ends_with(&format!(".{e}")));
    let is_txt = [
        "txt", "md", "json", "csv", "log", "py", "rs", "js", "ts", "html", "css", "xml", "yaml",
        "yml", "toml", "ini", "sh", "bat", "ps1",
    ]
    .iter()
    .any(|e| lower.ends_with(&format!(".{e}")));
    let mut data_url = String::new();
    let mut text = String::new();
    if is_img && size <= 15 * 1024 * 1024 {
        let bytes = std::fs::read(&p).map_err(|e| format!("读取图片失败：{e}"))?;
        let mime = match lower.rsplit('.').next().unwrap_or("") {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "svg" => "image/svg+xml",
            _ => "image/png",
        };
        use base64::Engine;
        data_url = format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    } else if is_txt && size <= 8 * 1024 * 1024 {
        text = std::fs::read_to_string(&p).unwrap_or_default();
        if text.chars().count() > 12000 {
            text = text.chars().take(12000).collect();
        }
    }
    Ok(Attachment {
        name,
        size,
        kind: if is_img {
            "image".into()
        } else if is_txt {
            "text".into()
        } else {
            "file".into()
        },
        data_url,
        text,
    })
}

/// 执行模型请求的工具调用。content 回填给模型；images 仅供前端展示。
#[tauri::command]
pub async fn execute_tool(
    app: tauri::AppHandle,
    state: State<'_, crate::AppState>,
    name: String,
    args_json: String,
) -> Result<ToolResult, String> {
    let args: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
    match name.as_str() {
        "run_command" => Ok(ToolResult::text(run_command(app, state, &args).await?)),
        "read_file" => Ok(ToolResult::text(read_file(&args).await?)),
        "web_search" => Ok(ToolResult::text(web_search(state, &args).await?)),
        "fetch_web" => Ok(ToolResult::text(fetch_web(state, &args).await?)),
        "list_desktop_images" => list_desktop_images(&args),
        _ => Err(format!("未知工具：{name}")),
    }
}

/// 在本机执行平台 shell 命令（仅返回输出文本，不交互）。
/// 这里是安全边界：即使前端绕过 UI 直接 invoke，也必须经过原生系统确认框。
async fn run_command(
    app: tauri::AppHandle,
    _state: State<'_, crate::AppState>,
    args: &Value,
) -> Result<String, String> {
    let cmd = args
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if cmd.trim().is_empty() {
        return Err("run_command: 缺少 command 参数".to_string());
    }
    if cmd.len() > 4000 {
        return Err("run_command: 命令过长（上限 4000 字符）".to_string());
    }

    let preview = cmd.clone();
    let parent = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口，无法显示命令确认框".to_string())?;
    let _ = parent.set_focus();

    // 使用 AsyncMessageDialog + parent：macOS 走 beginSheetModal 的异步 Completion，
    // 不会像同步 show() 那样同时调用 runModal 而提前返回默认按钮。
    let approved = rfd::AsyncMessageDialog::new()
        .set_parent(&parent)
        .set_level(rfd::MessageLevel::Warning)
        .set_title("LocalRun · 确认执行本机命令")
        .set_description(format!(
            "Agent 请求执行以下 shell 命令：\n\n{preview}\n\n是否仅允许本次执行？"
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        .await;

    if approved != rfd::MessageDialogResult::Yes {
        return Err("用户拒绝了本次命令执行".to_string());
    }

    // 阻塞型子进程放到线程池，避免卡 async 运行时
    let res = tauri::async_runtime::spawn_blocking(move || run_shell(&cmd))
        .await
        .map_err(|e| format!("执行线程错误：{e}"))??;
    Ok(format_shell_result(&res))
}

struct ShellResult {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 独立进程组：超时时可以连同 shell 派生的子进程一起终止。
        command.process_group(0);
    }
}

fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // 负 PID 表示进程组。process_group(0) 已让子进程成为组长。
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

fn read_capped<R: Read>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(limit.min(8192));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = limit.saturating_sub(out.len());
                if n > remaining {
                    truncated = true;
                }
                let take = n.min(remaining);
                out.extend_from_slice(&buf[..take]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    (out, truncated)
}

/// 平台 shell：Windows 用 PowerShell，macOS/Linux 用 sh -c。
/// stdout/stderr 并发读取并限流，超过 30 秒直接终止整个进程组。
fn run_shell(cmd: &str) -> Result<ShellResult, String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", cmd]);
        command
    };
    #[cfg(not(target_os = "windows"))]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", cmd]);
        command
    };

    configure_process_group(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        if cfg!(windows) {
            format!("启动 PowerShell 失败：{e}")
        } else {
            format!("启动 shell 失败：{e}")
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取命令 stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取命令 stderr".to_string())?;
    let out_handle = thread::spawn(move || read_capped(stdout, COMMAND_OUTPUT_LIMIT));
    let err_handle = thread::spawn(move || read_capped(stderr, COMMAND_OUTPUT_LIMIT));

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                kill_process_tree(&mut child);
                break child
                    .wait()
                    .map_err(|e| format!("等待超时命令退出失败：{e}"))?;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                kill_process_tree(&mut child);
                let _ = child.wait();
                return Err(format!("查询命令状态失败：{error}"));
            }
        }
    };

    let (stdout, stdout_truncated) = out_handle
        .join()
        .map_err(|_| "读取命令 stdout 线程异常".to_string())?;
    let (stderr, stderr_truncated) = err_handle
        .join()
        .map_err(|_| "读取命令 stderr 线程异常".to_string())?;
    Ok(ShellResult {
        status,
        stdout,
        stderr,
        timed_out,
        stdout_truncated,
        stderr_truncated,
    })
}

fn format_shell_result(result: &ShellResult) -> String {
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let mut out = String::new();
    if result.timed_out {
        out.push_str("命令执行超时（30 秒），已终止。\n");
    }
    if !result.status.success() {
        out.push_str(&format!(
            "命令执行失败（exit {}）：\n",
            result.status.code().unwrap_or(-1)
        ));
    }
    if !stdout.trim().is_empty() {
        out.push_str("stdout:\n");
        out.push_str(stdout.trim_end());
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("stderr:\n");
        out.push_str(stderr.trim_end());
        out.push('\n');
    }
    if out.trim().is_empty() {
        out.push_str("命令执行成功，无输出。");
    }
    if result.stdout_truncated || result.stderr_truncated {
        out.push_str("\n（命令输出超过 64 KiB，已截断）");
    }
    clip(&out)
}

fn image_mime(extension: &str) -> Option<&'static str> {
    match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// 列出用户桌面上的图片，返回给模型的文件清单和只用于 UI 的缩略图。
/// 不读取桌面以外的目录，不上传图片，也不把 base64 回填给模型。
fn list_desktop_images(args: &Value) -> Result<ToolResult, String> {
    const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(6)
        .clamp(1, 12) as usize;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "无法确定用户主目录".to_string())?;
    let dir = home.join("Desktop");
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("无法读取桌面目录 {}：{e}", dir.display()))?;

    let mut items: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(extension) = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_lowercase())
        else {
            continue;
        };
        if image_mime(&extension).is_none() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        items.push((modified, path, metadata.len()));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));

    let mut lines = vec![format!("桌面图片目录：{}", dir.display())];
    if items.is_empty() {
        lines.push("没有找到 png/jpg/jpeg/webp/gif/bmp 图片。".to_string());
        return Ok(ToolResult {
            content: lines.join("
"),
            images: Vec::new(),
        });
    }

    let total = items.len();
    let mut images = Vec::new();
    lines.push(format!(
        "共找到 {total} 张图片，显示最近 {} 张：",
        total.min(limit)
    ));
    for (index, (_, path, size)) in items.iter().take(limit).enumerate() {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        if *size > MAX_IMAGE_BYTES {
            lines.push(format!(
                "{}. {}（{} MB，过大，未在聊天中显示）",
                index + 1,
                name,
                size / 1024 / 1024
            ));
            continue;
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase();
        match std::fs::read(path) {
            Ok(bytes) => {
                use base64::Engine;
                let data_url = format!(
                    "data:{};base64,{}",
                    image_mime(&extension).unwrap_or("image/png"),
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                );
                images.push(ToolImage {
                    name: name.clone(),
                    path: path.display().to_string(),
                    size: *size,
                    data_url,
                });
                lines.push(format!(
                    "{}. {}（{} KB）",
                    index + 1,
                    name,
                    (*size).div_ceil(1024)
                ));
            }
            Err(error) => lines.push(format!("{}. {}（读取失败：{error}）", index + 1, name)),
        }
    }
    if images.is_empty() {
        lines.push("没有可显示的图片（文件可能过大或读取失败）。".to_string());
    } else {
        lines.push(
            "缩略图已由客户端显示给用户。当前本地模型没有视觉能力，不能读取图片像素内容，不要猜测图片内容。"
                .to_string(),
        );
    }
    Ok(ToolResult {
        content: clip(&lines.join("
")),
        images,
    })
}

/// 读取本地文本文件（大小保护，默认前 200 行）
async fn read_file(args: &Value) -> Result<String, String> {
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
        .replace('/', &sep);
    let max_lines = args
        .get("max_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(2000) as usize;
    if path.trim().is_empty() {
        return Err("read_file: 缺少 path 参数".to_string());
    }
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("read_file: 无法访问 {path}：{e}"))?;
    if meta.len() > 8 * 1024 * 1024 {
        return Err(format!(
            "read_file: 文件过大（{} MB），已拒绝读取",
            meta.len() / 1024 / 1024
        ));
    }
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read_file: 读取失败：{e}"))?;
    let lines: Vec<&str> = content.lines().collect();
    let shown: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    let mut out = shown.join("\n");
    if lines.len() > max_lines {
        out.push_str(&format!(
            "\n…（共 {} 行，仅显示前 {} 行）",
            lines.len(),
            max_lines
        ));
    }
    Ok(clip(&out))
}

/// 联网搜索：抓取 Bing 搜索结果页（走系统代理），解析标题/链接/摘要，返回格式化文本
async fn web_search(state: State<'_, crate::AppState>, args: &Value) -> Result<String, String> {
    let q = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if q.trim().is_empty() {
        return Err("web_search: 缺少 query 参数".to_string());
    }
    let _ = state; // 预留
    let client = crate::net::build_client(Some(std::time::Duration::from_secs(20)))
        .map_err(|e| format!("构建客户端失败：{e}"))?;
    let url = format!(
        "https://www.bing.com/search?q={}&mkt=zh-CN&count=6",
        urlencode(&q)
    );
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .send()
        .await
        .map_err(|e| format!("搜索失败（网络/代理）：{e}"))?;
    let html = resp
        .text()
        .await
        .map_err(|e| format!("读取搜索页失败：{e}"))?;

    let results = parse_bing(&html);
    if results.is_empty() {
        // Bing 反爬或空结果：自动切换 DuckDuckGo HTML 版再试一次
        let ddg_url = format!("https://html.duckduckgo.com/html/?q={}", urlencode(&q));
        let resp2 = client
            .get(&ddg_url)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
            .header("Accept-Language", "zh-CN,zh;q=0.9")
            .send()
            .await;
        if let Ok(r2) = resp2 {
            if let Ok(html2) = r2.text().await {
                let ddg = parse_ddg(&html2);
                if !ddg.is_empty() {
                    let mut lines = vec![format!("DuckDuckGo 搜索“{q}”结果（{} 条）：", ddg.len())];
                    for (i, (title, link, snippet)) in ddg.iter().enumerate() {
                        lines.push(format!("{}. {title}\n   链接：{link}\n   {snippet}", i + 1));
                    }
                    return Ok(lines.join("\n"));
                }
            }
        }
        return Ok(format!(
            "Bing 与 DuckDuckGo 均未返回“{q}”的搜索结果（可能被反爬拦截或查询无结果）。"
        ));
    }
    let mut lines = vec![format!("Bing 搜索“{q}”结果（{} 条）：", results.len())];
    for (i, (title, link, snippet)) in results.iter().enumerate() {
        lines.push(format!("{}. {title}\n   链接：{link}\n   {snippet}", i + 1));
    }
    Ok(lines.join("\n"))
}

/// 读网页：抓取指定 URL 的页面文本（去标签），返回前 6000 字符（走系统代理）
async fn fetch_web(state: State<'_, crate::AppState>, args: &Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if url.trim().is_empty() {
        return Err("fetch_web: 缺少 url 参数".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("fetch_web: 仅支持 http/https 链接".to_string());
    }
    let _ = state; // 预留
    let client = crate::net::build_client(Some(std::time::Duration::from_secs(25)))
        .map_err(|e| format!("构建客户端失败：{e}"))?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .send()
        .await
        .map_err(|e| format!("抓取页面失败（网络/代理/被拦截）：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("抓取页面失败：HTTP {}", resp.status()));
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("读取页面失败：{e}"))?;
    // 粗提取正文文本：去 script/style/标签，压缩空白
    let body = strip_html_body(&html);
    if body.trim().is_empty() {
        return Ok("页面抓取成功但未提取到可见文本（可能是 JS 渲染页面）。".to_string());
    }
    Ok(clip(&body))
}

/// 简单 HTML → 文本：去 script/style/注释与标签，压缩空白
fn strip_html_body(html: &str) -> String {
    let mut s = String::with_capacity(html.len());
    let mut rest = html;
    let mut skip_depth = 0usize;
    while !rest.is_empty() {
        if let Some(lt) = rest.find('<') {
            s.push_str(&rest[..lt]);
            let tag_start = lt;
            let tag_end = rest[tag_start..]
                .find('>')
                .map(|e| tag_start + e + 1)
                .unwrap_or(rest.len());
            let tag = &rest[tag_start..tag_end.min(rest.len())];
            let lower = tag.to_lowercase();
            if lower.starts_with("<!--") {
                // 注释：找到 -->
                if let Some(ce) = lower.find("-->") {
                    rest = &rest[tag_end.min(rest.len())..];
                    let _ = ce;
                    continue;
                }
                rest = &rest[tag_end.min(rest.len())..];
                continue;
            }
            if lower.starts_with("<script") || lower.starts_with("<style") {
                skip_depth += 1;
                rest = &rest[tag_end.min(rest.len())..];
                continue;
            }
            if (lower.starts_with("</script") || lower.starts_with("</style")) && skip_depth > 0 {
                skip_depth -= 1;
                rest = &rest[tag_end.min(rest.len())..];
                continue;
            }
            if skip_depth == 0 {
                s.push(' ');
            }
            rest = &rest[tag_end.min(rest.len())..];
        } else {
            s.push_str(rest);
            break;
        }
    }
    // 压缩空白
    let mut out = String::new();
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// 解析 Bing 结果页：按 b_algo 条目提取 标题/链接/摘要
fn parse_bing(html: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let marker = r#"<li class="b_algo""#;
    let mut rest = html;
    while let Some(pos) = rest.find(marker) {
        let seg_start = pos;
        let seg_end = rest[seg_start..]
            .find(r#"</li>"#)
            .map(|e| seg_start + e)
            .unwrap_or(rest.len());
        let seg = &rest[seg_start..seg_end.min(rest.len())];
        // 标题：h2 里的链接文本
        let title = if let Some(a_pos) = seg.find(r#"<h2>"#) {
            let a_sub = &seg[a_pos..];
            let a_end = a_sub.find(r#"</a>"#).unwrap_or(a_sub.len());
            let link = extract_attr(&a_sub[..a_end], "href");
            let t = strip_tags(&a_sub[..a_end]);
            (t, link)
        } else {
            (String::new(), String::new())
        };
        // 摘要：<p ...>...</p>
        let snippet = if let Some(p_pos) = seg.find("<p ") {
            let p_sub = &seg[p_pos..];
            let p_end = p_sub.find("</p>").unwrap_or(p_sub.len());
            strip_tags(&p_sub[..p_end])
        } else {
            String::new()
        };
        if !title.0.is_empty() {
            let link = if title.1.starts_with("http") {
                title.1.clone()
            } else {
                String::new()
            };
            out.push((title.0, link, snippet));
        }
        rest = &rest[seg_end..];
    }
    out
}

/// 解析 DuckDuckGo HTML 版结果页：result__a 标题/链接 + result__snippet 摘要
fn parse_ddg(html: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find(r#"class="result__a""#) {
        let seg_start = rest[..pos].rfind("<a").unwrap_or(pos);
        let a_end = rest[pos..]
            .find("</a>")
            .map(|e| pos + e)
            .unwrap_or(rest.len());
        let a_seg = &rest[seg_start..a_end.min(rest.len())];
        let raw_link = extract_attr(a_seg, "href");
        let link = decode_ddg(&raw_link);
        let title = strip_tags(a_seg);
        let after = &rest[a_end..];
        let snippet = if let Some(spos) = after.find(r#"class="result__snippet""#) {
            let s_start = after[..spos].rfind("<a").unwrap_or(spos);
            let s_end = after[spos..]
                .find("</a>")
                .map(|e| spos + e)
                .unwrap_or(after.len());
            strip_tags(&after[s_start..s_end.min(after.len())])
        } else {
            String::new()
        };
        if !title.is_empty() {
            out.push((title, link, snippet));
        }
        rest = &rest[a_end..];
    }
    out
}

/// 还原 DuckDuckGo /l/?uddg= 重定向为真实链接
fn decode_ddg(link: &str) -> String {
    if let Some(u) = link.find("uddg=") {
        let v: String = link[u + 5..].chars().take_while(|c| *c != '&').collect();
        let bytes = v.as_bytes();
        let mut dec = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(n) = u8::from_str_radix(&v[i + 1..i + 3], 16) {
                    dec.push(n);
                    i += 3;
                    continue;
                }
            }
            dec.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&dec).to_string()
    } else {
        link.to_string()
    }
}

/// 提取属性值：name="..."
fn extract_attr(seg: &str, name: &str) -> String {
    let pat = format!(r#"{name}=""#);
    seg.find(&pat)
        .map(|p| {
            let s = p + pat.len();
            seg[s..]
                .find('"')
                .map(|e| seg[s..s + e].to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// 去 HTML 标签与常见实体
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn clip(s: &str) -> String {
    let c: String = s.chars().take(6000).collect();
    if c.chars().count() < s.chars().count() {
        format!("{}…（输出已截断）", c)
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::read_capped;

    #[test]
    fn output_reader_caps_and_marks_truncation() {
        let data = vec![b'x'; 100];
        let (out, truncated) = read_capped(&data[..], 10);
        assert_eq!(out.len(), 10);
        assert!(truncated);

        let (out, truncated) = read_capped(&b"short"[..], 10);
        assert_eq!(out, b"short");
        assert!(!truncated);
    }
}
