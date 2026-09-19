use anyhow::{Context, Result};
use std::path::Path;

/// Move a managed book directory to the system Trash. On macOS the crate's
/// default Finder/AppleScript method can wait indefinitely for automation
/// permission; NSFileManager performs the same move without Finder IPC.
pub fn move_to_trash(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};

        let mut context = trash::TrashContext::new();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context
            .delete(path)
            .with_context(|| format!("无法将 {} 移到废纸篓", path.display()))?;
    }

    #[cfg(not(target_os = "macos"))]
    trash::delete(path).with_context(|| format!("无法将 {} 移到废纸篓", path.display()))?;

    Ok(())
}

pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("文件已不存在：{}", path.display());
    }

    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .status()
            .context("无法打开 Finder")?;
        if !status.success() {
            anyhow::bail!("Finder 未能显示文件（退出状态：{status}）");
        }
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let status = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .status()
            .context("无法打开文件资源管理器")?;
        if !status.success() {
            anyhow::bail!("文件资源管理器未能显示文件（退出状态：{status}）");
        }
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let folder = path.parent().unwrap_or(path);
        let status = std::process::Command::new("xdg-open")
            .arg(folder)
            .status()
            .context("无法打开文件管理器")?;
        if !status.success() {
            anyhow::bail!("文件管理器未能打开目录（退出状态：{status}）");
        }
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    anyhow::bail!("当前平台不支持打开文件管理器");

    #[allow(unreachable_code)]
    Ok(())
}
