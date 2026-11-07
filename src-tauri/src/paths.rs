use std::path::PathBuf;

/// 与 Tauri app_data_dir 默认规则保持一致，供不便传递 AppHandle 的后台模块复用。
pub fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = std::env::var_os("APPDATA") {
            return PathBuf::from(root).join("com.localrun.app");
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(root).join("com.localrun.app");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("com.localrun.app");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(root).join("com.localrun.app");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("com.localrun.app");
        }
    }

    std::env::temp_dir().join("com.localrun.app")
}
