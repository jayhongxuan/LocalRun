//! 网络公共模块：系统代理读取 + 统一 HTTP 客户端构造。
//! 本机网络环境特殊：DNS 对 huggingface.co 的直连解析被污染（解析到错误 IP 会挂死），
//! 而系统代理（如 v2rayN 127.0.0.1:10808）通畅。因此所有外连默认走系统代理。

use std::time::Duration;

/// 读系统代理：Windows 读注册表（HKCU Internet Settings），macOS/Linux 读环境变量 HTTP(S)_PROXY
pub fn system_proxy_url() -> Option<String> {
    system_proxy_url_impl()
}

#[cfg(target_os = "windows")]
fn system_proxy_url_impl() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let server = server.trim();
    if server.is_empty() || server.eq_ignore_ascii_case("auto") {
        return None;
    }
    // ProxyServer 可能是 "127.0.0.1:10808" 或多协议 "http=...;https=..."
    if server.find('=').is_some() {
        // 多协议形式：取 https= 或 http= 段
        for part in server.split(';') {
            if let Some((k, v)) = part.split_once('=') {
                let k = k.trim().to_ascii_lowercase();
                let v = v.trim();
                if !v.is_empty() && (k == "http" || k == "https") {
                    return Some(format!("http://{v}"));
                }
            }
        }
        return None;
    }
    Some(format!("http://{server}"))
}

#[cfg(not(target_os = "windows"))]
fn system_proxy_url_impl() -> Option<String> {
    // macOS/Linux：优先 HTTPS_PROXY，其次 HTTP_PROXY（常见的 http://host:port 或 host:port）
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if v.is_empty() || v.eq_ignore_ascii_case("auto") {
                continue;
            }
            let url = if v.starts_with("http://") || v.starts_with("https://") {
                v
            } else {
                format!("http://{v}")
            };
            return Some(url);
        }
    }
    None
}

/// 本地专用客户端（不走代理，仅用于 127.0.0.1 健康检查等）
pub fn build_local_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("LocalRun/0.1 (self-hosted model manager)")
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败: {e}"))
}

/// 直连客户端（不走任何系统代理）——用于国内镜像（hf-mirror、ModelScope 等）与必须直连的场景
pub fn build_direct_client(timeout: Option<Duration>) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .user_agent("LocalRun/0.1 (self-hosted model manager)")
        .connect_timeout(Duration::from_secs(15));
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build().map_err(|e| format!("HTTP 客户端初始化失败: {e}"))
}

/// 该镜像地址是否需要直连（不走系统代理）：
/// hf-mirror / ModelScope 等国内镜像直连最快，且海外代理节点常常访问它们失败。
pub fn mirror_use_direct(mirror: &str) -> bool {
    let m = mirror.to_ascii_lowercase();
    m.contains("hf-mirror") || m.contains("hf_mirror") || m.contains("modelscope")
}

/// 统一构造 HTTP 客户端：schannel/native-tls + 系统代理；timeout=None 时不设总超时（下载用）
pub fn build_client(timeout: Option<Duration>) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .user_agent("LocalRun/0.1 (self-hosted model manager)")
        .connect_timeout(Duration::from_secs(15));
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    if let Some(p) = system_proxy_url() {
        if let Ok(proxy) = reqwest::Proxy::all(&p) {
            b = b.proxy(proxy);
        }
    }
    b.build().map_err(|e| format!("HTTP 客户端初始化失败: {e}"))
}
