//! 硬件检测：NVIDIA GPU、Apple Silicon Metal 统一内存、CPU、内存和磁盘。

use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct GpuInfo {
    pub present: bool,
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: u64,
    pub driver: String,
    #[serde(default)]
    pub unified_memory: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HardwareInfo {
    pub gpu: GpuInfo,
    pub cpu_name: String,
    pub cores: usize,
    pub threads: usize,
    pub ram_total_gb: u64,
    pub ram_available_gb: u64,
    pub disk_total_gb: u64,
    pub disk_free_gb: u64,
}

fn query_nvidia_smi() -> Option<GpuInfo> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.used,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 4 {
        return None;
    }
    Some(GpuInfo {
        present: true,
        name: parts[0].to_string(),
        vram_total_mb: parts[1].parse().unwrap_or(0),
        vram_used_mb: parts[2].parse().unwrap_or(0),
        driver: parts[3].to_string(),
        unified_memory: false,
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn query_apple_gpu(available_memory_bytes: u64) -> Option<GpuInfo> {
    Some(GpuInfo {
        present: true,
        name: "Apple Silicon GPU".to_string(),
        // Apple GPU 使用统一内存；这里用当前可用系统内存作保守估算，不把总内存误称为独立显存。
        vram_total_mb: available_memory_bytes / (1024 * 1024),
        vram_used_mb: 0,
        driver: "Metal · 统一内存（估算）".to_string(),
        unified_memory: true,
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn query_apple_gpu(_available_memory_bytes: u64) -> Option<GpuInfo> {
    None
}

#[tauri::command]
pub fn get_hardware_info() -> HardwareInfo {
    use sysinfo::{Disks, System};

    let mut sys = System::new_all();
    sys.refresh_all();

    let gpu = query_nvidia_smi()
        .or_else(|| query_apple_gpu(sys.available_memory()))
        .unwrap_or_default();

    let cpu_name = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "未知".to_string());
    let cores = sys.physical_core_count().unwrap_or(0);
    let threads = sys.cpus().len();

    let disks = Disks::new_with_refreshed_list();
    let mut disk_total: u64 = 0;
    let mut disk_free: u64 = 0;
    for d in disks.list() {
        disk_total += d.total_space();
        disk_free += d.available_space();
    }

    HardwareInfo {
        gpu,
        cpu_name,
        cores,
        threads,
        ram_total_gb: sys.total_memory() / (1024 * 1024 * 1024),
        ram_available_gb: sys.available_memory() / (1024 * 1024 * 1024),
        disk_total_gb: disk_total / (1024 * 1024 * 1024),
        disk_free_gb: disk_free / (1024 * 1024 * 1024),
    }
}
