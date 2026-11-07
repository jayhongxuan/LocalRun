//! 推理引擎（llama.cpp）一键安装。
//! 流程：GitHub API 查最新 release → 按本机 GPU 选资产（CUDA 12.4 / CPU）→
//! 走系统代理下载（Range 断点续传）→ 安全解压 → 校验 llama-server.exe → 状态持久化。
//! 进度通过 "engine-progress" 事件推给前端。

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

const GH_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases";
const GH_DL: &str = "https://github.com/ggml-org/llama.cpp/releases/download";
/// API 限流时的回退 tag（资产直链不消耗 API 配额；安装成功后会把最新 tag 缓存到文件）
const FALLBACK_TAG: &str = "b11976";

#[derive(Serialize, Deserialize, Clone)]
pub struct EngineStatus {
    pub state: String, // none | downloading | installed | error
    pub version: String,
    pub variant: String, // cuda-12.4 | cpu
    pub exe: String,
    pub msg: String,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Serialize, Clone)]
struct EngineProgress {
    stage: String, // fetch | download | unzip | done | error
    percent: f32,
    msg: String,
}

fn engine_dir() -> PathBuf {
    crate::paths::app_data_dir().join("engine")
}

fn engine_json_path() -> PathBuf {
    engine_dir().join("engine.json")
}

pub fn load_status() -> EngineStatus {
    let p = engine_json_path();
    match fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str::<EngineStatus>(&s).unwrap_or(EngineStatus {
            state: "none".into(),
            version: String::new(),
            variant: String::new(),
            exe: String::new(),
            msg: "状态文件损坏".into(),
        }),
        Err(_) => EngineStatus {
            state: "none".into(),
            version: String::new(),
            variant: String::new(),
            exe: String::new(),
            msg: String::new(),
        },
    }
}

fn emit_progress(app: &AppHandle, stage: &str, percent: f32, msg: &str) {
    let _ = app.emit(
        "engine-progress",
        EngineProgress {
            stage: stage.into(),
            percent,
            msg: msg.into(),
        },
    );
}

fn tag_cache_path() -> PathBuf {
    engine_dir().join("last_tag.txt")
}

fn read_cached_tag() -> Option<String> {
    fs::read_to_string(tag_cache_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_cached_tag(tag: &str) {
    let _ = fs::write(tag_cache_path(), tag);
}

/// 平台资产文件名（Windows: llama-{tag}-bin-win-{chosen}-x64.zip；macOS: llama-{tag}-bin-macos-{arch}.tar.gz）
fn asset_name(tag: &str, _chosen: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("llama-{tag}-bin-win-{_chosen}-x64.zip")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x64"
        };
        format!("llama-{tag}-bin-macos-{arch}.tar.gz")
    }
}

/// 平台关键字（API 资产名过滤用）
fn platform_key() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "win"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "macos"
    }
}

/// 平台压缩包扩展名
fn archive_ext() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        ".zip"
    }
    #[cfg(not(target_os = "windows"))]
    {
        ".tar.gz"
    }
}

/// 推理服务可执行文件名（Windows 带 .exe，macOS 无扩展名）
fn server_bin_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "llama-server.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "llama-server"
    }
}

/// 查询 llama.cpp 最新 release 资产（按资产名模糊匹配 variant）
async fn fetch_latest_asset(variant: &str) -> Result<(String, String, u64), String> {
    // 优先走 API；限流（403/429）时回退到已知 tag 直链（不耗 API 配额）
    match fetch_latest_asset_api(variant).await {
        Ok(x) => Ok(x),
        Err(_e) => {
            let tag = read_cached_tag().unwrap_or_else(|| FALLBACK_TAG.to_string());
            let name = asset_name(&tag, variant);
            let url = format!("{GH_DL}/{tag}/{name}");
            // size 未知（0）→ 下载时用 Content-Length
            Ok((tag, url, 0))
        }
    }
}

async fn fetch_latest_asset_api(variant: &str) -> Result<(String, String, u64), String> {
    let client = crate::net::build_client(Some(std::time::Duration::from_secs(20)))?;
    let resp = client
        .get(format!("{GH_API}?per_page=1"))
        .send()
        .await
        .map_err(|e| format!("连接 GitHub 失败: {e}"))?;
    if resp.status() == reqwest::StatusCode::FORBIDDEN
        || resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err("GitHub API 限流".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("GitHub API HTTP {}", resp.status()));
    }
    let releases: Vec<GhRelease> = resp
        .json()
        .await
        .map_err(|e| format!("解析 GitHub 响应失败: {e}"))?;
    let rel = releases
        .first()
        .ok_or_else(|| "GitHub 返回空 release 列表".to_string())?;
    let tag = rel.tag_name.clone();
    let want = asset_name(&tag, variant);
    let asset = rel
        .assets
        .iter()
        .find(|a| a.name == want)
        .or_else(|| {
            // 宽松匹配：llama-{tag}-bin-{platform_key}-{variant}*.{archive_ext}
            rel.assets.iter().find(|a| {
                a.name
                    .starts_with(&format!("llama-{tag}-bin-{}", platform_key()))
                    && a.name.ends_with(archive_ext())
            })
        })
        .ok_or_else(|| {
            format!(
                "未找到资产 {want}（可用：{}）",
                rel.assets
                    .iter()
                    .filter(|a| a.name.contains(platform_key()) && a.name.ends_with(archive_ext()))
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    Ok((tag, asset.browser_download_url.clone(), asset.size))
}

/// 通用下载（Range 断点续传），进度回调（已下载字节, 总字节）
async fn download_url(
    app: &AppHandle,
    url: &str,
    dest_part: &Path,
    total_hint: u64,
) -> Result<(), String> {
    use futures_util::StreamExt;
    let client = crate::net::build_client(None)?;
    let offset = fs::metadata(dest_part).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if offset > 0 {
        req = req.header("Range", format!("bytes={offset}-"));
    }
    let resp = req.send().await.map_err(|e| format!("下载连接失败: {e}"))?;
    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!("下载失败：HTTP {}", resp.status()));
    }
    let total = if total_hint > 0 {
        total_hint
    } else {
        offset + resp.content_length().unwrap_or(0)
    };
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dest_part)
        .map_err(|e| format!("无法创建下载文件: {e}"))?;
    let mut stream = resp.bytes_stream();
    let mut done = offset;
    let mut last_emit = std::time::Instant::now();
    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| format!("传输中断: {e}"))?;
        f.write_all(&chunk).map_err(|e| format!("写入失败: {e}"))?;
        done += chunk.len() as u64;
        if last_emit.elapsed().as_millis() >= 150 {
            emit_progress(
                app,
                "download",
                (done as f32 / total.max(1) as f32 * 100.0).min(99.0),
                &format!("{}/{}", done, total),
            );
            last_emit = std::time::Instant::now();
        }
    }
    f.flush().ok();
    Ok(())
}

/// 安全解压（按平台：Windows zip / macOS tar.gz），拒绝绝对路径、..、盘符
fn safe_extract(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        safe_unzip(archive_path, target_dir)
    }
    #[cfg(not(target_os = "windows"))]
    {
        safe_untar_gz(archive_path, target_dir)
    }
}

#[cfg(target_os = "windows")]
fn safe_unzip(zip_path: &Path, target_dir: &Path) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析 zip 失败: {e}"))?;
    let count = archive.len();
    for i in 0..count {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读 zip 条目失败: {e}"))?;
        let raw = entry.name().replace('\\', "/");
        // 安全校验
        if raw.split('/').any(|seg| seg == ".." || seg.is_empty()) || raw.starts_with('/') {
            continue; // 跳过可疑路径（llama.cpp 官方包不含，防御性）
        }
        if raw.ends_with('/') {
            fs::create_dir_all(target_dir.join(&raw)).map_err(|e| format!("创建目录失败: {e}"))?;
            continue;
        }
        let out_path = target_dir.join(&raw);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        let mut out = File::create(&out_path).map_err(|e| format!("创建文件失败: {e}"))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("解压失败: {e}"))?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn safe_link_destination(
    target_dir: &Path,
    link_path: &Path,
    link_target: &Path,
) -> Option<PathBuf> {
    use std::path::Component;

    if link_target.is_absolute() {
        return None;
    }
    let mut stack: Vec<Component<'_>> = link_path.parent()?.components().collect();
    for component in link_target.components() {
        match component {
            Component::Normal(part) => stack.push(Component::Normal(part)),
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop()?;
            }
            _ => return None,
        }
    }
    let destination: PathBuf = stack.iter().collect();
    if destination.starts_with(target_dir) {
        Some(destination)
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn safe_untar_gz(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|e| format!("打开 tar.gz 失败: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    for entry in archive
        .entries()
        .map_err(|e| format!("读取 tar 条目失败: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("读 tar 条目失败: {e}"))?;
        let raw = entry
            .path()
            .map_err(|e| format!("读路径失败: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        if raw.split('/').any(|seg| seg == ".." || seg.is_empty()) || raw.starts_with('/') {
            continue; // 防御性跳过可疑路径
        }
        let out_path = target_dir.join(&raw);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败: {e}"))?;
        } else if kind.is_file() {
            entry
                .unpack(&out_path)
                .map_err(|e| format!("解压失败: {e}"))?;
        } else if kind.is_symlink() {
            // llama.cpp 的 macOS 包里，libllama-common.0.dylib 等是相对符号链接。
            // 之前直接跳过符号链接，导致 llama-server 启动时报 Library not loaded。
            let link_target = match entry
                .link_name()
                .map_err(|e| format!("读取符号链接失败: {e}"))?
            {
                Some(target) => target.into_owned(),
                None => continue,
            };
            if safe_link_destination(target_dir, &out_path, &link_target).is_none() {
                continue; // 拒绝绝对路径或逃逸到目标目录之外的链接
            }
            if out_path.exists() || out_path.symlink_metadata().is_ok() {
                let _ = fs::remove_file(&out_path);
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link_target, &out_path)
                .map_err(|e| format!("创建符号链接失败: {e}"))?;
        }
        // 其他类型（设备、FIFO、硬链接等）跳过
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).map_err(|e| format!("读取可执行文件权限失败: {e}"))?;
    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        let mut permissions = metadata.permissions();
        permissions.set_mode(mode | 0o755);
        fs::set_permissions(path, permissions).map_err(|e| format!("设置执行权限失败: {e}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn find_exe(dir: &Path) -> Option<PathBuf> {
    // 顶层或一层子目录里找推理服务可执行文件
    let name = server_bin_name();
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path().join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 解析安装变体：auto → 平台默认（Windows 按 GPU 选 cuda/cpu；macOS/Linux 固定 macos）
fn resolve_variant(variant: &str) -> String {
    if variant == "auto" || variant.is_empty() {
        #[cfg(target_os = "windows")]
        {
            let hw = crate::hardware::get_hardware_info();
            if hw.gpu.present {
                "cuda-12.4".to_string()
            } else {
                "cpu".to_string()
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            "macos".to_string()
        }
    } else {
        variant.to_string()
    }
}

/// 安装引擎：variant = "auto"（按本机 GPU 选）| "cuda-12.4" | "cpu"
#[tauri::command]
pub async fn install_engine(app: AppHandle, variant: String) -> Result<(), String> {
    let chosen = resolve_variant(&variant);
    emit_progress(&app, "fetch", 0.0, &format!("查询最新版本（{chosen}）…"));
    let (tag, url, size) = fetch_latest_asset(&chosen).await?;
    emit_progress(&app, "fetch", 5.0, &format!("最新版本 {tag}"));

    let dir = engine_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建引擎目录失败: {e}"))?;
    let arc_part = dir.join(format!("llama-{tag}-{chosen}.part"));
    let arc_path = dir.join(format!("llama-{tag}-{chosen}{}", archive_ext()));

    // 已下载完整则跳过下载
    if !arc_path.exists() {
        emit_progress(
            &app,
            "download",
            5.0,
            &format!("下载 llama.cpp {tag}（{:.0} MB）…", size as f64 / 1e6),
        );
        download_url(&app, &url, &arc_part, size).await?;
        fs::rename(&arc_part, &arc_path).map_err(|e| format!("下载文件改名失败: {e}"))?;
    }

    // 解压到独立目录
    let target = dir.join(format!("llama-{tag}-{chosen}"));
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| format!("清理旧引擎失败: {e}"))?;
    }
    emit_progress(&app, "unzip", 90.0, "解压中…");
    safe_extract(&arc_path, &target)?;

    let exe = find_exe(&target).ok_or_else(|| format!("解压后未找到 {}", server_bin_name()))?;
    ensure_executable(&exe)?;
    // 清理压缩包（已解压）
    let _ = fs::remove_file(&arc_path);

    let status = EngineStatus {
        state: "installed".into(),
        version: tag.clone(),
        variant: chosen.clone(),
        exe: exe.display().to_string(),
        msg: format!("llama.cpp {tag}（{chosen}）已安装"),
    };
    let json = serde_json::to_string_pretty(&status).map_err(|e| format!("序列化失败: {e}"))?;
    fs::write(engine_json_path(), json).map_err(|e| format!("写引擎状态失败: {e}"))?;
    write_cached_tag(&tag);
    emit_progress(&app, "done", 100.0, &status.msg);
    Ok(())
}

/// 查询当前引擎状态（读本地状态文件）
#[tauri::command]
pub fn get_engine_status() -> EngineStatus {
    load_status()
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn temp_root(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "localrun-engine-test-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn tar_relative_symlink_is_preserved() {
        let root = temp_root("symlink");
        let archive = root.join("engine.tar.gz");
        {
            let file = File::create(&archive).unwrap();
            let gz = GzEncoder::new(file, Compression::default());
            let mut builder = tar::Builder::new(gz);

            let mut file_header = tar::Header::new_gnu();
            file_header.set_size(3);
            file_header.set_mode(0o644);
            builder
                .append_data(&mut file_header, "pkg/lib.1.dylib", &b"lib"[..])
                .unwrap();

            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Symlink);
            link_header.set_size(0);
            link_header.set_mode(0o777);
            builder
                .append_link(&mut link_header, "pkg/lib.dylib", "lib.1.dylib")
                .unwrap();

            builder.into_inner().unwrap().finish().unwrap();
        }

        let target = root.join("out");
        fs::create_dir_all(&target).unwrap();
        safe_untar_gz(&archive, &target).unwrap();
        let link = target.join("pkg/lib.dylib");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("lib.1.dylib")
        );
        assert_eq!(fs::read(&link).unwrap(), b"lib");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn symlink_target_outside_target_dir_is_rejected() {
        let target = PathBuf::from("/tmp/localrun-target");
        let link = target.join("pkg/lib.dylib");
        assert!(safe_link_destination(&target, &link, Path::new("../../etc/passwd")).is_none());
        assert!(safe_link_destination(&target, &link, Path::new("/etc/passwd")).is_none());
        assert!(safe_link_destination(&target, &link, Path::new("lib.1.dylib")).is_some());
    }
}
