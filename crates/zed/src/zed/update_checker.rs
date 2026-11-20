use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use futures::AsyncReadExt as _;
use gpui::{App, PromptLevel};
use http_client::{http, AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy};
use serde::Deserialize;
use workspace::notifications::NotificationId;
use workspace::{Toast, with_active_or_new_workspace};

#[allow(dead_code)]
pub const REPO_OWNER: &str = "lexusblack";
#[allow(dead_code)]
pub const REPO_NAME: &str = "phpworkbench";
pub const VERSION_RAW_URL: &str = "https://raw.githubusercontent.com/lexusblack/phpworkbench/main/VERSION";
pub const RELEASES_API_URL: &str = "https://api.github.com/repos/lexusblack/phpworkbench/releases/latest";
pub const RELEASES_WEB_URL: &str = "https://github.com/lexusblack/phpworkbench/releases";

struct UpdateNotification;

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub current_version: semver::Version,
    pub latest_version: semver::Version,
    pub release_url: String,
    pub release_title: Option<String>,
    pub asset_url: Option<String>,
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Deserialize, Debug)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

pub fn current_version() -> anyhow::Result<semver::Version> {
    let version_str = option_env!("PHPWORKBENCH_VERSION").unwrap_or("1.0.30");
    let trimmed = version_str.trim().trim_start_matches('v');
    semver::Version::parse(trimmed)
        .map_err(|e| anyhow::anyhow!("Failed to parse current version '{}': {}", version_str, e))
}

pub fn find_platform_asset(assets: &[GitHubAsset], version: &semver::Version) -> (String, String, Option<u64>) {
    #[cfg(target_os = "macos")]
    {
        if let Some(asset) = assets.iter().find(|a| a.name.ends_with(".dmg")) {
            return (asset.browser_download_url.clone(), asset.name.clone(), Some(asset.size));
        }
        let name = format!("PhpWorkbench-{}.dmg", version);
        let url = format!("{}/download/v{}/{}", RELEASES_WEB_URL, version, name);
        (url, name, None)
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(asset) = assets.iter().find(|a| a.name.ends_with(".exe")) {
            return (asset.browser_download_url.clone(), asset.name.clone(), Some(asset.size));
        }
        let name = format!("PhpWorkbench-Setup-{}.exe", version);
        let url = format!("{}/download/v{}/{}", RELEASES_WEB_URL, version, name);
        (url, name, None)
    }

    #[cfg(target_os = "linux")]
    {
        #[cfg(target_arch = "x86_64")]
        let deb_suffix = "amd64.deb";
        #[cfg(target_arch = "aarch64")]
        let deb_suffix = "arm64.deb";
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let deb_suffix = ".deb";

        if let Some(asset) = assets.iter().find(|a| a.name.ends_with(deb_suffix)) {
            return (asset.browser_download_url.clone(), asset.name.clone(), Some(asset.size));
        }
        if let Some(asset) = assets.iter().find(|a| a.name.ends_with(".deb")) {
            return (asset.browser_download_url.clone(), asset.name.clone(), Some(asset.size));
        }
        let name = format!("phpworkbench_{}_{}", version, deb_suffix);
        let url = format!("{}/download/v{}/{}", RELEASES_WEB_URL, version, name);
        (url, name, None)
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let name = format!("phpworkbench-{}", version);
        let url = format!("{}/tag/v{}", RELEASES_WEB_URL, version);
        (url, name, None)
    }
}

pub async fn check_for_updates(http: Arc<dyn HttpClient>) -> anyhow::Result<Option<UpdateInfo>> {
    let current = current_version()?;

    // 1. Try GitHub Releases API first to get rich metadata (assets, release notes, release URL)
    let api_result: anyhow::Result<Option<UpdateInfo>> = async {
        let request = http::Request::builder()
            .method("GET")
            .uri(RELEASES_API_URL)
            .header("User-Agent", format!("PhpWorkbench/{}", current))
            .header("Accept", "application/vnd.github+json")
            .follow_redirects(RedirectPolicy::FollowAll)
            .body(AsyncBody::default())?;

        let response = http.send(request).await?;
        let status = response.status();
        if status.is_success() {
            let mut body_str = String::new();
            response.into_body().read_to_string(&mut body_str).await?;
            let release: GitHubRelease = serde_json::from_str(&body_str)?;
            let remote_tag = release.tag_name.trim().trim_start_matches('v');
            if let Ok(latest_ver) = semver::Version::parse(remote_tag) {
                if latest_ver > current {
                    let (asset_url, asset_name, asset_size) = find_platform_asset(&release.assets, &latest_ver);
                    return Ok(Some(UpdateInfo {
                        current_version: current.clone(),
                        latest_version: latest_ver,
                        release_url: release.html_url,
                        release_title: release.name,
                        asset_url: Some(asset_url),
                        asset_name: Some(asset_name),
                        asset_size,
                    }));
                } else {
                    return Ok(None);
                }
            }
        }
        anyhow::bail!("GitHub API returned {}", status)
    }
    .await;

    if let Ok(info) = api_result {
        return Ok(info);
    }

    // 2. Fallback to raw VERSION file from main branch (never rate-limited, always up-to-date)
    let raw_request = http::Request::builder()
        .method("GET")
        .uri(VERSION_RAW_URL)
        .header("User-Agent", format!("PhpWorkbench/{}", current))
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(AsyncBody::default())?;

    let response = http.send(raw_request).await?;
    if response.status().is_success() {
        let mut version_text = String::new();
        response.into_body().read_to_string(&mut version_text).await?;
        let remote_clean = version_text.trim().trim_start_matches('v');
        if let Ok(latest_ver) = semver::Version::parse(remote_clean) {
            if latest_ver > current {
                let (asset_url, asset_name, asset_size) = find_platform_asset(&[], &latest_ver);
                return Ok(Some(UpdateInfo {
                    current_version: current,
                    latest_version: latest_ver.clone(),
                    release_url: format!("{}/tag/v{}", RELEASES_WEB_URL, latest_ver),
                    release_title: None,
                    asset_url: Some(asset_url),
                    asset_name: Some(asset_name),
                    asset_size,
                }));
            } else {
                return Ok(None);
            }
        }
    }

    anyhow::bail!("Could not retrieve latest version from GitHub")
}

pub fn start_download_and_install(update: UpdateInfo, http: Arc<dyn HttpClient>, cx: &mut App) {
    let latest_ver = update.latest_version.clone();
    let asset_url = match update.asset_url.clone() {
        Some(url) => url,
        None => {
            cx.open_url(&update.release_url);
            return;
        }
    };
    let asset_name = update.asset_name.clone().unwrap_or_else(|| format!("PhpWorkbench-{}", latest_ver));

    let ver_toast = latest_ver.clone();
    with_active_or_new_workspace(cx, move |workspace, _window, cx| {
        let msg = format!("Downloading PhpWorkbench v{}...", ver_toast);
        workspace.show_toast(
            Toast::new(NotificationId::unique::<UpdateNotification>(), msg),
            cx,
        );
    });

    cx.spawn(async move |cx| {
        let temp_dir = std::env::temp_dir();
        let dest_file = temp_dir.join(&asset_name);
        let part_file = temp_dir.join(format!("{}.part", asset_name));

        let download_res: anyhow::Result<()> = async {
            let request = http::Request::builder()
                .method("GET")
                .uri(&asset_url)
                .header("User-Agent", "PhpWorkbench-AutoUpdater")
                .follow_redirects(RedirectPolicy::FollowAll)
                .body(AsyncBody::default())?;

            let response = http.send(request).await?;
            if !response.status().is_success() {
                anyhow::bail!("Download failed with status: {}", response.status());
            }

            let mut body = response.into_body();
            let mut file = std::fs::File::create(&part_file)?;
            let mut buf = [0u8; 64 * 1024];

            loop {
                let n = body.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
            }
            file.flush()?;
            drop(file);

            if dest_file.exists() {
                std::fs::remove_file(&dest_file).ok();
            }
            std::fs::rename(&part_file, &dest_file)?;
            Ok(())
        }
        .await;

        match download_res {
            Ok(()) => {
                cx.update(|cx| {
                    with_active_or_new_workspace(cx, move |workspace, _window, cx| {
                        let final_dest = dest_file.clone();
                        let success_msg = format!("PhpWorkbench v{} is ready to install!", latest_ver);
                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<UpdateNotification>(), success_msg)
                                .on_click("Restart to Update", move |_window, cx| {
                                    apply_update_and_restart(&final_dest, cx);
                                }),
                            cx,
                        );
                    });
                });
            }
            Err(e) => {
                log::error!("Failed to download update: {}", e);
                std::fs::remove_file(&part_file).ok();
                cx.update(|cx| {
                    with_active_or_new_workspace(cx, move |workspace, _window, cx| {
                        let rel_url = update.release_url.clone();
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<UpdateNotification>(),
                                format!("Auto-update download failed. Click to open releases page."),
                            )
                            .on_click("Open Browser", move |_window, cx| {
                                cx.open_url(&rel_url);
                            }),
                            cx,
                        );
                    });
                });
            }
        }
    })
    .detach();
}

pub fn apply_update_and_restart(installer_path: &Path, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = apply_macos_update(installer_path) {
            log::error!("Failed to apply macOS update: {}", e);
            return;
        }
        cx.quit();
    }

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = apply_windows_update(installer_path) {
            log::error!("Failed to apply Windows update: {}", e);
            return;
        }
        cx.quit();
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = apply_linux_update(installer_path) {
            log::error!("Failed to apply Linux update: {}", e);
            return;
        }
        cx.quit();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        log::info!("Update downloaded to: {:?}", installer_path);
    }
}

#[cfg(target_os = "macos")]
fn get_macos_target_app() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(contents_macos) = exe.parent() {
            if let Some(contents) = contents_macos.parent() {
                if let Some(app_bundle) = contents.parent() {
                    if app_bundle.extension().and_then(|s| s.to_str()) == Some("app") {
                        return app_bundle.to_path_buf();
                    }
                }
            }
        }
    }
    PathBuf::from("/Applications/PhpWorkbench.app")
}

#[cfg(target_os = "macos")]
fn apply_macos_update(dmg_path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let target_app = get_macos_target_app();
    let pid = std::process::id();
    let script_path = std::env::temp_dir().join("phpworkbench_updater.sh");

    let script = r#"#!/bin/bash
PID="$1"
DMG_PATH="$2"
TARGET_APP="$3"

# 1. Wait for current PhpWorkbench process to terminate
while kill -0 "$PID" 2>/dev/null; do
    sleep 0.2
done

# 2. Mount DMG to temporary mount point
MOUNT_DIR=$(mktemp -d /tmp/pw_update_mount.XXXXXX)
if hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT_DIR" "$DMG_PATH" >/dev/null 2>&1; then
    if [ -d "$MOUNT_DIR/PhpWorkbench.app" ]; then
        # 3. Replace target application bundle
        rm -rf "$TARGET_APP"
        cp -R "$MOUNT_DIR/PhpWorkbench.app" "$TARGET_APP"
    fi
    hdiutil detach "$MOUNT_DIR" -force >/dev/null 2>&1 || true
    rm -rf "$MOUNT_DIR"
fi

rm -f "$DMG_PATH"

# 4. Relaunch updated PhpWorkbench
open "$TARGET_APP"
rm -f "$0"
"#;

    std::fs::write(&script_path, script)?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;

    std::process::Command::new("/bin/bash")
        .arg(&script_path)
        .arg(pid.to_string())
        .arg(dmg_path)
        .arg(&target_app)
        .spawn()?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_windows_update(exe_path: &Path) -> anyhow::Result<()> {
    let pid = std::process::id();
    let script_path = std::env::temp_dir().join("phpworkbench_updater.bat");
    let current_exe = std::env::current_exe()?;

    let script = format!(
        "@echo off\r\n\
        :wait\r\n\
        tasklist /FI \"PID eq {}\" 2>NUL | find /I \"{}\" >NUL\r\n\
        if \"%ERRORLEVEL%\"==\"0\" (\r\n\
            timeout /t 1 /nobreak >NUL\r\n\
            goto wait\r\n\
        )\r\n\
        \"{}\" /S\r\n\
        timeout /t 2 /nobreak >NUL\r\n\
        start \"\" \"{}\"\r\n\
        del \"{}\"\r\n\
        del \"%~f0\"\r\n",
        pid,
        pid,
        exe_path.to_string_lossy(),
        current_exe.to_string_lossy(),
        exe_path.to_string_lossy()
    );

    std::fs::write(&script_path, script)?;

    std::process::Command::new("cmd")
        .args(&["/C", &script_path.to_string_lossy()])
        .spawn()?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_linux_update(pkg_path: &Path) -> anyhow::Result<()> {
    if pkg_path.extension().and_then(|s| s.to_str()) == Some("deb") {
        std::process::Command::new("pkexec")
            .arg("dpkg")
            .arg("-i")
            .arg(pkg_path)
            .spawn()
            .or_else(|_| {
                std::process::Command::new("xdg-open")
                    .arg(pkg_path)
                    .spawn()
            })?;
    } else {
        std::process::Command::new("xdg-open")
            .arg(pkg_path)
            .spawn()?;
    }
    Ok(())
}

pub fn init(http: Arc<dyn HttpClient>, cx: &mut App) {
    cx.spawn(async move |cx| {
        // Wait 5 seconds after startup so IDE loads instantly without interference
        cx.background_executor().timer(Duration::from_secs(5)).await;

        if let Ok(Some(update)) = check_for_updates(http.clone()).await {
            cx.update(|cx| {
                with_active_or_new_workspace(cx, move |workspace, _window, cx| {
                    let msg = format!("PhpWorkbench v{} is available!", update.latest_version);
                    let update_clone = update.clone();
                    let http_clone = http.clone();
                    workspace.show_toast(
                        Toast::new(NotificationId::unique::<UpdateNotification>(), msg)
                            .on_click("Download & Install", move |_window, cx| {
                                start_download_and_install(update_clone.clone(), http_clone.clone(), cx);
                            }),
                        cx,
                    );
                });
            });
        }
    })
    .detach();
}

pub fn check_for_updates_interactive(cx: &mut App) {
    let http = cx.http_client();
    let current_str = option_env!("PHPWORKBENCH_VERSION").unwrap_or("1.0.30").to_string();

    cx.spawn(async move |cx| {
        let check_result = check_for_updates(http.clone()).await;

        cx.update(|cx| {
            with_active_or_new_workspace(cx, move |_workspace, window, cx| {
                match check_result {
                    Ok(Some(update)) => {
                        let size_str = update.asset_size.map(|s| format!(" (~{} MB)", s / (1024 * 1024))).unwrap_or_default();
                        let title_suffix = update.release_title.as_deref().map(|t| format!(" - {}", t)).unwrap_or_default();
                        let prompt = window.prompt(
                            PromptLevel::Info,
                            &format!("New version v{} available!{}", update.latest_version, title_suffix),
                            Some(&format!(
                                "A newer version of PhpWorkbench (v{}) is available{}.\nYou currently have v{}.\n\nWould you like to download and install the update now?",
                                update.latest_version, size_str, update.current_version
                            )),
                            &["Download & Install", "View in Browser", "Later"],
                            cx,
                        );
                        let update_clone = update.clone();
                        let http_clone = http.clone();
                        cx.spawn(async move |_, cx| {
                            match prompt.await {
                                Ok(0) => {
                                    cx.update(|cx| {
                                        start_download_and_install(update_clone, http_clone, cx);
                                    });
                                }
                                Ok(1) => {
                                    cx.update(|cx| {
                                        cx.open_url(&update_clone.release_url);
                                    });
                                }
                                _ => {}
                            }
                        })
                        .detach();
                    }
                    Ok(None) => {
                        let prompt = window.prompt(
                            PromptLevel::Info,
                            "PhpWorkbench is up to date",
                            Some(&format!(
                                "PhpWorkbench v{} is currently the newest version available.",
                                current_str
                            )),
                            &["OK"],
                            cx,
                        );
                        cx.spawn(async move |_, _| {
                            prompt.await.ok();
                        })
                        .detach();
                    }
                    Err(e) => {
                        let prompt = window.prompt(
                            PromptLevel::Warning,
                            "Check for Updates",
                            Some(&format!(
                                "Could not connect to GitHub to check for updates: {}\n\nYou can manually visit the releases page.",
                                e
                            )),
                            &["Open Releases Page", "Close"],
                            cx,
                        );
                        cx.spawn(async move |_, cx| {
                            if prompt.await == Ok(0) {
                                cx.update(|cx| {
                                    cx.open_url(RELEASES_WEB_URL);
                                });
                            }
                        })
                        .detach();
                    }
                }
            });
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_version_parsing() {
        let ver = current_version().unwrap();
        assert!(ver >= semver::Version::new(1, 0, 0));
    }

    #[test]
    fn test_find_platform_asset() {
        let v = semver::Version::new(1, 0, 31);
        let assets = vec![
            GitHubAsset {
                name: "PhpWorkbench-1.0.31.dmg".into(),
                browser_download_url: "https://example.com/dmg".into(),
                size: 100_000_000,
            },
            GitHubAsset {
                name: "PhpWorkbench-Setup-1.0.31.exe".into(),
                browser_download_url: "https://example.com/exe".into(),
                size: 80_000_000,
            },
            GitHubAsset {
                name: "phpworkbench_1.0.31_amd64.deb".into(),
                browser_download_url: "https://example.com/deb".into(),
                size: 70_000_000,
            },
        ];

        let (url, name, size) = find_platform_asset(&assets, &v);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(name, "PhpWorkbench-1.0.31.dmg");
            assert_eq!(url, "https://example.com/dmg");
            assert_eq!(size, Some(100_000_000));
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(name, "PhpWorkbench-Setup-1.0.31.exe");
            assert_eq!(url, "https://example.com/exe");
            assert_eq!(size, Some(80_000_000));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(name.ends_with(".deb"));
        }
    }
}
