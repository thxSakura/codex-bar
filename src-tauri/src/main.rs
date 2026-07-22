#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

const WINDOW_WIDTH: f64 = 470.0;
const WINDOW_HEIGHT: f64 = 420.0;
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 300;
const APP_SERVER_TIMEOUT_SECS: u64 = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    id: String,
    title: String,
    used_percent: f64,
    window_minutes: Option<u32>,
    reset_at: Option<DateTime<Utc>>,
    reset_description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostSnapshot {
    balance: f64,
    currency: String,
    label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSnapshot {
    account_email: Option<String>,
    plan_type: Option<String>,
    login_method: Option<String>,
    primary: RateWindow,
    secondary: Option<RateWindow>,
    model_specific: Option<RateWindow>,
    extra_rate_windows: Vec<RateWindow>,
    credits: Option<CostSnapshot>,
    updated_at: DateTime<Utc>,
    source: String,
    stale: bool,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeatmapDay {
    date: String,
    value: u64,
    intensity: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    refresh_interval_secs: u64,
    autostart: bool,
    privacy_mode: bool,
    hide_account: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            refresh_interval_secs: DEFAULT_REFRESH_INTERVAL_SECS,
            autostart: false,
            privacy_mode: true,
            hide_account: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    refresh_interval_secs: Option<u64>,
    autostart: Option<bool>,
    privacy_mode: Option<bool>,
    hide_account: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppCache {
    usage: Option<UsageSnapshot>,
    heatmap: Option<Vec<HeatmapDay>>,
    settings: AppSettings,
}

struct AppState {
    cache: Mutex<AppCache>,
}

fn main() {
    #[cfg(target_os = "macos")]
    let _ = fix_path_env::fix();

    tauri::Builder::default()
        .manage(AppState {
            cache: Mutex::new(load_cache()),
        })
        .invoke_handler(tauri::generate_handler![
            get_usage,
            refresh_usage,
            get_heatmap,
            get_settings,
            update_settings
        ])
        .setup(|app| {
            let window = create_window(app.handle())?;
            attach_focus_handler(&window);
            create_tray(app.handle())?;
            spawn_refresh_loop(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Codex Usage Tray");
}

fn create_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Codex Usage")
        .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .skip_taskbar(true)
        .visible(false)
        .build()
}

fn attach_focus_handler(window: &WebviewWindow) {
    let popup = window.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Focused(false)) {
            let _ = popup.hide();
        }
    });
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let refresh = MenuItem::with_id(app, "refresh", "刷新", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    #[cfg(windows)]
    let autostart = MenuItem::with_id(app, "autostart", "切换开机启动", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    #[cfg(windows)]
    let menu = Menu::with_items(app, &[&refresh, &settings, &autostart, &quit])?;
    #[cfg(not(windows))]
    let menu = Menu::with_items(app, &[&refresh, &settings, &quit])?;

    TrayIconBuilder::with_id("codex-usage")
        .tooltip("Codex Usage")
        .icon(tray_image()?)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                let _ = toggle_popup(&app);
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "refresh" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = refresh_usage_inner(&app, false).await;
                });
            }
            "settings" => {
                let _ = show_popup(app);
                let _ = app.emit("open-settings", ());
            }
            #[cfg(windows)]
            "autostart" => {
                let _ = toggle_autostart_from_menu(app);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn tray_image() -> tauri::Result<Image<'static>> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let dx = x as f32 - 16.0;
            let dy = y as f32 - 16.0;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= 14.0 { 255 } else { 0 };
            let (r, g, b) = if dist < 8.0 {
                (28, 214, 187)
            } else {
                (32, 77, 98)
            };
            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    Ok(Image::new_owned(rgba, 32, 32))
}

fn toggle_popup(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    if window.is_visible()? {
        window.hide()?;
    } else {
        show_popup(app)?;
    }
    Ok(())
}

fn show_popup(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    position_popup(&window)?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

fn position_popup(window: &WebviewWindow) -> tauri::Result<()> {
    let cursor = window
        .cursor_position()
        .unwrap_or(PhysicalPosition::new(WINDOW_WIDTH, WINDOW_HEIGHT));
    let monitor = window
        .current_monitor()?
        .or_else(|| window.primary_monitor().ok().flatten());
    let (left, top, right, bottom) = if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let pos = area.position;
        let size = area.size;
        (
            pos.x as f64,
            pos.y as f64,
            pos.x as f64 + size.width as f64,
            pos.y as f64 + size.height as f64,
        )
    } else {
        (0.0, 0.0, 1920.0, 1080.0)
    };

    let x = (cursor.x - WINDOW_WIDTH + 18.0).clamp(left + 8.0, right - WINDOW_WIDTH - 8.0);
    let y = (cursor.y - WINDOW_HEIGHT - 12.0).clamp(top + 8.0, bottom - WINDOW_HEIGHT - 8.0);
    window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
}

fn spawn_refresh_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last_refresh = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let interval = app
                .state::<AppState>()
                .cache
                .lock()
                .ok()
                .map(|cache| cache.settings.refresh_interval_secs.max(60))
                .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS);
            if last_refresh.elapsed() >= Duration::from_secs(interval) {
                let _ = refresh_usage_inner(&app, false).await;
                last_refresh = Instant::now();
            }
        }
    });
}

#[tauri::command]
async fn get_usage(app: AppHandle) -> Result<UsageSnapshot, String> {
    let cached = app
        .state::<AppState>()
        .cache
        .lock()
        .map_err(|_| "缓存锁定失败".to_string())?
        .usage
        .clone();

    if let Some(usage) = cached {
        Ok(usage)
    } else {
        refresh_usage_inner(&app, false).await
    }
}

#[tauri::command]
async fn refresh_usage(app: AppHandle) -> Result<UsageSnapshot, String> {
    refresh_usage_inner(&app, false).await
}

async fn refresh_usage_inner(app: &AppHandle, quiet: bool) -> Result<UsageSnapshot, String> {
    let result = fetch_usage(app).await;
    let state = app.state::<AppState>();
    let mut cache = state.cache.lock().map_err(|_| "缓存锁定失败".to_string())?;

    let snapshot = match result {
        Ok(snapshot) => {
            cache.usage = Some(snapshot.clone());
            snapshot
        }
        Err(error) => {
            let mut stale = cache.usage.clone().unwrap_or_else(|| empty_usage(&error));
            stale.stale = true;
            stale.error = Some(error);
            cache.usage = Some(stale.clone());
            stale
        }
    };
    save_cache(&cache);
    drop(cache);

    if !quiet {
        let _ = app.emit("usage-refreshed", snapshot.clone());
    }
    Ok(snapshot)
}

async fn fetch_usage(_app: &AppHandle) -> Result<UsageSnapshot, String> {
    tokio::task::spawn_blocking(fetch_usage_via_app_server)
        .await
        .map_err(|e| format!("Codex app-server 任务失败: {e}"))?
}

fn fetch_usage_via_app_server() -> Result<UsageSnapshot, String> {
    let mut child = spawn_codex_app_server()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "无法打开 Codex app-server stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法打开 Codex app-server stdout".to_string())?;

    let messages = [
        serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "win-codex-usage-tray",
                    "title": "Codex Usage Tray",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
        serde_json::json!({"method": "initialized"}),
        serde_json::json!({"id": 2, "method": "account/read", "params": {"refreshToken": false}}),
        serde_json::json!({"id": 3, "method": "account/rateLimits/read", "params": {}}),
    ];

    for message in messages {
        writeln!(stdin, "{message}").map_err(|e| format!("写入 Codex app-server 请求失败: {e}"))?;
    }
    stdin
        .flush()
        .map_err(|e| format!("刷新 Codex app-server stdin 失败: {e}"))?;

    let (line_tx, line_rx) = mpsc::channel::<Result<Option<String>, String>>();
    let reader_handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = line_tx.send(Ok(None));
                    break;
                }
                Ok(_) => {
                    if line_tx.send(Ok(Some(line))).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = line_tx.send(Err(format!("读取 Codex app-server 响应失败: {error}")));
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(APP_SERVER_TIMEOUT_SECS);
    let mut account: Option<Value> = None;
    let mut rate_limits: Option<Value> = None;
    let mut last_error: Option<String> = None;
    let mut timed_out = false;

    while account.is_none() || rate_limits.is_none() {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }

        let wait_for = (deadline - now).min(Duration::from_millis(250));
        let line = match line_rx.recv_timeout(wait_for) {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Err(error)) => {
                stop_app_server_child(&mut child, reader_handle);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(error) = value.get("error") {
            last_error = Some(error.to_string());
        }
        match value.get("id").and_then(Value::as_i64) {
            Some(2) => account = response_payload(&value),
            Some(3) => rate_limits = response_payload(&value),
            _ => {}
        }
    }

    let exited = child.try_wait().ok().flatten();

    let Some(rate_limits) = rate_limits else {
        let hint = codex_app_server_help();
        let error = if timed_out {
            last_error.unwrap_or_else(|| {
                format!("{hint} {APP_SERVER_TIMEOUT_SECS} 秒内没有收到完整响应。")
            })
        } else if let Some(status) = exited {
            last_error.unwrap_or_else(|| format!("{hint} codex 进程退出状态: {status}"))
        } else {
            last_error.unwrap_or_else(|| hint.to_string())
        };
        stop_app_server_child(&mut child, reader_handle);
        return Err(error);
    };
    stop_app_server_child(&mut child, reader_handle);
    Ok(build_usage_snapshot_from_app_server(
        account.as_ref(),
        &rate_limits,
        None,
    ))
}

fn stop_app_server_child(child: &mut Child, reader_handle: std::thread::JoinHandle<()>) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader_handle.join();
}

fn spawn_codex_app_server() -> Result<std::process::Child, String> {
    let mut command = codex_app_server_command()?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|e| {
        format!(
            "启动 `codex app-server` 失败: {e}。{}",
            codex_app_server_help()
        )
    })
}

fn codex_app_server_help() -> &'static str {
    #[cfg(windows)]
    {
        "请先在普通 PowerShell 运行 `codex app-server`；如果看到 Access is denied，请不要使用 WindowsApps 包内 codex.exe，改用可执行的 Codex CLI wrapper 或设置 CODEX_BIN。"
    }
    #[cfg(target_os = "macos")]
    {
        "请先在 Terminal 运行 `which codex` 和 `codex app-server`；如果找不到命令，请安装 Codex CLI，或设置 CODEX_BIN 指向 Codex CLI 的绝对路径。"
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        "请先在终端运行 `codex app-server`；如果找不到命令，请安装 Codex CLI，或设置 CODEX_BIN 指向 Codex CLI 的绝对路径。"
    }
}

fn codex_app_server_command() -> Result<Command, String> {
    let mut command = Command::new(resolve_codex_executable()?);
    command.arg("app-server");
    Ok(command)
}

#[cfg(windows)]
fn resolve_codex_executable() -> Result<PathBuf, String> {
    if let Ok(value) = std::env::var("CODEX_BIN") {
        let path = normalize_codex_bin_path(PathBuf::from(value.trim().trim_matches('"')));
        if !path.exists() {
            return Err(format!("CODEX_BIN 指向的文件不存在: {}", path.display()));
        }
        if is_windowsapps_path(&path) {
            return Err(format!(
                "CODEX_BIN 指向 WindowsApps 包内程序，普通桌面程序无法直接启动: {}",
                path.display()
            ));
        }
        return Ok(path);
    }

    if let Some(path) = find_in_path("codex.exe")
        .or_else(|| find_in_path("codex.cmd"))
        .or_else(|| find_in_path("codex.bat"))
        .or_else(find_openai_codex_runtime)
    {
        return Ok(path);
    }

    Err("未找到可直接执行的 Codex CLI。官方 app-server 启动方式是 `codex app-server`，请安装 Codex CLI，或设置 CODEX_BIN 指向 `codex.exe` / `codex.cmd`；如果使用 Codex 桌面版，可指向 `%LOCALAPPDATA%\\OpenAI\\Codex\\bin\\<版本>\\codex.exe`，不要指向 WindowsApps 包内路径。".to_string())
}

#[cfg(not(windows))]
fn resolve_codex_executable() -> Result<PathBuf, String> {
    if let Ok(value) = std::env::var("CODEX_BIN") {
        let path = PathBuf::from(value.trim().trim_matches('"'));
        if path.exists() {
            return Ok(path);
        }
        return Err(format!("CODEX_BIN 指向的文件不存在: {}", path.display()));
    }
    find_in_path("codex").ok_or_else(|| {
        "未找到可直接执行的 Codex CLI。请先在终端运行 `which codex`；如果找不到命令，请安装 Codex CLI，或设置 CODEX_BIN 指向 Codex CLI 的绝对路径。".to_string()
    })
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            candidate.is_file() && {
                #[cfg(windows)]
                {
                    !is_windowsapps_path(candidate)
                }
                #[cfg(not(windows))]
                {
                    true
                }
            }
        })
}

#[cfg(windows)]
fn normalize_codex_bin_path(path: PathBuf) -> PathBuf {
    if path.is_dir() {
        path.join("codex.exe")
    } else {
        path
    }
}

#[cfg(windows)]
fn find_openai_codex_runtime() -> Option<PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)?
        .join("OpenAI")
        .join("Codex")
        .join("bin");
    let entries = fs::read_dir(root).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let candidate = entry.path().join("codex.exe");
            if !candidate.exists() || is_windowsapps_path(&candidate) {
                return None;
            }
            let modified = fs::metadata(&candidate)
                .and_then(|metadata| metadata.modified())
                .ok();
            Some((modified, candidate))
        })
        .max_by_key(|(modified, path)| (*modified, path.clone()))
        .map(|(_, path)| path)
}

fn is_windowsapps_path(path: &Path) -> bool {
    let path = path.to_string_lossy().to_ascii_lowercase();
    path.contains("\\windowsapps\\openai.codex_")
        || path.contains("\\microsoft\\windowsapps\\codex.exe")
}

fn response_payload(value: &Value) -> Option<Value> {
    value
        .get("result")
        .cloned()
        .or_else(|| value.get("params").cloned())
        .or_else(|| value.get("payload").cloned())
}

fn build_usage_snapshot(json: &Value, account_email: Option<String>) -> UsageSnapshot {
    let plan_type = json
        .get("plan_type")
        .or_else(|| json.get("planType"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let rate_limit = json
        .get("rate_limit")
        .or_else(|| json.get("rateLimits"))
        .or_else(|| {
            (json.get("primary").is_some()
                || json.get("secondary").is_some()
                || json.get("primary_window").is_some()
                || json.get("secondary_window").is_some())
            .then_some(json)
        });
    let primary = rate_limit
        .and_then(|limits| limits.get("primary_window"))
        .or_else(|| rate_limit.and_then(|limits| limits.get("primary")))
        .map(|value| parse_window("codex-primary", "5小时", value))
        .or_else(|| {
            json.get("rate_limits")
                .or_else(|| json.get("rateLimits"))
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .map(|value| parse_window("codex-primary", "5小时", value))
        })
        .unwrap_or_else(|| {
            RateWindow::simple(
                "codex-primary",
                "Codex",
                json.get("used_percent")
                    .or_else(|| json.get("usage_percent"))
                    .and_then(json_f64)
                    .unwrap_or(0.0),
            )
        });

    let secondary = rate_limit
        .and_then(|limits| limits.get("secondary_window"))
        .or_else(|| rate_limit.and_then(|limits| limits.get("secondary")))
        .map(|value| parse_window("codex-weekly", "7天", value))
        .or_else(|| {
            json.get("rate_limits")
                .or_else(|| json.get("rateLimits"))
                .and_then(Value::as_array)
                .and_then(|items| items.get(1))
                .map(|value| parse_window("codex-weekly", "7天", value))
        });

    let model_specific = rate_limit
        .and_then(|limits| limits.get("code_review_window"))
        .or_else(|| rate_limit.and_then(|limits| limits.get("codeReview")))
        .or_else(|| rate_limit.and_then(|limits| limits.get("code_review")))
        .map(|value| parse_window("codex-code-review", "Code Review", value));

    UsageSnapshot {
        account_email,
        login_method: plan_type.as_deref().map(plan_label),
        plan_type,
        primary,
        secondary,
        model_specific,
        extra_rate_windows: extract_additional_rate_limits(json),
        credits: extract_credits(json),
        updated_at: Utc::now(),
        source: "app-server".to_string(),
        stale: false,
        error: None,
    }
}

fn build_usage_snapshot_from_app_server(
    account: Option<&Value>,
    rate_limits: &Value,
    usage: Option<&Value>,
) -> UsageSnapshot {
    let payload = rate_limits
        .get("rateLimits")
        .or_else(|| rate_limits.get("rate_limits"))
        .unwrap_or(rate_limits);
    let account_email = account.and_then(extract_account_email);
    let mut snapshot = build_usage_snapshot(payload, account_email);
    if snapshot.plan_type.is_none() {
        snapshot.plan_type = account.and_then(extract_account_plan_type);
        snapshot.login_method = snapshot.plan_type.as_deref().map(plan_label);
    }
    if snapshot.credits.is_none() {
        snapshot.credits = usage
            .and_then(extract_credits)
            .or_else(|| extract_credits(rate_limits));
    }
    snapshot
        .extra_rate_windows
        .extend(extract_rate_limits_by_id(rate_limits));
    dedupe_rate_windows(&mut snapshot.extra_rate_windows);
    snapshot
}

fn extract_account_email(account: &Value) -> Option<String> {
    account
        .get("email")
        .or_else(|| {
            account
                .get("account")
                .and_then(|account| account.get("email"))
        })
        .or_else(|| account.get("user").and_then(|user| user.get("email")))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn extract_account_plan_type(account: &Value) -> Option<String> {
    account
        .get("planType")
        .or_else(|| account.get("plan_type"))
        .or_else(|| {
            account
                .get("account")
                .and_then(|account| account.get("planType"))
        })
        .or_else(|| {
            account
                .get("account")
                .and_then(|account| account.get("plan_type"))
        })
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

impl RateWindow {
    fn simple(id: &str, title: &str, used_percent: f64) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            used_percent,
            window_minutes: None,
            reset_at: None,
            reset_description: None,
        }
    }
}

fn parse_window(id: &str, title: &str, value: &Value) -> RateWindow {
    let used_percent = value
        .get("used_percent")
        .or_else(|| value.get("usedPercent"))
        .or_else(|| value.get("usage_percent"))
        .or_else(|| value.get("usagePercent"))
        .and_then(json_f64)
        .unwrap_or(0.0);
    let window_minutes = value
        .get("limit_window_seconds")
        .and_then(Value::as_i64)
        .map(|seconds| (seconds / 60) as u32)
        .or_else(|| {
            value
                .get("windowDurationMins")
                .or_else(|| value.get("window_duration_mins"))
                .and_then(Value::as_i64)
                .map(|minutes| minutes as u32)
        });
    let reset_at = value
        .get("reset_at")
        .or_else(|| value.get("resetsAt"))
        .or_else(|| value.get("resetAt"))
        .and_then(Value::as_i64)
        .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
        .or_else(|| {
            value
                .get("resetsAt")
                .or_else(|| value.get("resetAt"))
                .and_then(Value::as_str)
                .and_then(parse_datetime)
        });

    RateWindow {
        id: id.to_string(),
        title: title.to_string(),
        used_percent,
        window_minutes,
        reset_at,
        reset_description: reset_at.map(format_reset_countdown),
    }
}

fn extract_additional_rate_limits(json: &Value) -> Vec<RateWindow> {
    json.get("additional_rate_limits")
        .or_else(|| json.get("additionalRateLimits"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_additional_rate_limit)
        .collect()
}

fn extract_rate_limits_by_id(json: &Value) -> Vec<RateWindow> {
    let Some(map) = json
        .get("rateLimitsByLimitId")
        .or_else(|| json.get("rate_limits_by_limit_id"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(id, value)| {
            if id.eq_ignore_ascii_case("codex") {
                return None;
            }
            let name = value
                .get("limitName")
                .or_else(|| value.get("limit_name"))
                .and_then(Value::as_str)
                .unwrap_or(id);
            let window = value
                .get("primary")
                .or_else(|| value.get("secondary"))
                .or_else(|| value.get("primary_window"))
                .or_else(|| value.get("secondary_window"))
                .unwrap_or(value);
            if window.as_object().is_some_and(|object| object.is_empty()) {
                return None;
            }
            Some(parse_window(
                &format!("codex-{}", slugify(id)),
                &titleize(name),
                window,
            ))
        })
        .collect()
}

fn dedupe_rate_windows(windows: &mut Vec<RateWindow>) {
    let mut seen = std::collections::HashSet::new();
    windows.retain(|window| seen.insert(window.id.clone()));
}

fn parse_additional_rate_limit(entry: &Value) -> Option<RateWindow> {
    let feature = entry
        .get("metered_feature")
        .or_else(|| entry.get("meteredFeature"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let limit_name = entry
        .get("limit_name")
        .or_else(|| entry.get("limitName"))
        .and_then(Value::as_str)
        .unwrap_or(feature);
    let rate_limit = entry
        .get("rate_limit")
        .or_else(|| entry.get("rateLimit"))
        .unwrap_or(entry);
    let window = rate_limit
        .get("primary_window")
        .or_else(|| rate_limit.get("primary"))
        .or_else(|| rate_limit.get("secondary_window"))
        .or_else(|| rate_limit.get("secondary"))?;
    if window.as_object().is_some_and(|object| object.is_empty()) {
        return None;
    }
    let mut parsed = parse_window(
        &format!("codex-{}", slugify(limit_name)),
        &titleize(limit_name),
        window,
    );
    if feature.eq_ignore_ascii_case("codex_spark")
        || limit_name.to_ascii_lowercase().contains("spark")
    {
        let is_weekly = parsed
            .window_minutes
            .is_some_and(|minutes| minutes >= 7 * 24 * 60)
            || rate_limit.get("secondary_window").is_some();
        parsed.id = if is_weekly {
            "codex-spark-weekly".to_string()
        } else {
            "codex-spark".to_string()
        };
        parsed.title = if is_weekly {
            "Codex Spark Weekly".to_string()
        } else {
            "Codex Spark 5-hour".to_string()
        };
    }
    Some(parsed)
}

fn extract_credits(json: &Value) -> Option<CostSnapshot> {
    let credits = json
        .get("credits")
        .or_else(|| json.get("creditBalance"))
        .or_else(|| json.get("credit_balance"))?;
    let has_credits = credits
        .get("has_credits")
        .or_else(|| credits.get("hasCredits"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let unlimited = credits
        .get("unlimited")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !has_credits || unlimited {
        return None;
    }
    Some(CostSnapshot {
        balance: credits
            .get("balance")
            .or_else(|| credits.get("remaining"))
            .or_else(|| credits.get("remainingBalance"))
            .and_then(json_f64)
            .unwrap_or(0.0),
        currency: "USD".to_string(),
        label: "Credits".to_string(),
    })
}

fn empty_usage(error: &str) -> UsageSnapshot {
    UsageSnapshot {
        account_email: None,
        plan_type: None,
        login_method: None,
        primary: RateWindow::simple("codex-primary", "Codex", 0.0),
        secondary: None,
        model_specific: None,
        extra_rate_windows: Vec::new(),
        credits: None,
        updated_at: Utc::now(),
        source: "none".to_string(),
        stale: true,
        error: Some(error.to_string()),
    }
}

#[tauri::command]
fn get_heatmap(app: AppHandle, days: Option<u32>) -> Result<Vec<HeatmapDay>, String> {
    let days = days.unwrap_or(210).clamp(1, 366);
    let heatmap = build_heatmap(days);
    if let Ok(mut cache) = app.state::<AppState>().cache.lock() {
        cache.heatmap = Some(heatmap.clone());
        save_cache(&cache);
    }
    Ok(heatmap)
}

fn build_heatmap(days: u32) -> Vec<HeatmapDay> {
    let today = Utc::now().date_naive();
    let start = today - ChronoDuration::days((days - 1) as i64);
    let mut values: HashMap<String, u64> = HashMap::new();
    if let Some(root) = codex_sessions_root() {
        scan_codex_sessions(&root, start, today, &mut values);
    }
    let max = values.values().copied().max().unwrap_or(0);
    (0..days)
        .map(|offset| {
            let date = start + ChronoDuration::days(offset as i64);
            let key = date.format("%Y-%m-%d").to_string();
            let value = values.get(&key).copied().unwrap_or(0);
            HeatmapDay {
                date: key,
                value,
                intensity: heat_intensity(value, max),
            }
        })
        .collect()
}

fn scan_codex_sessions(
    root: &Path,
    start: NaiveDate,
    end: NaiveDate,
    values: &mut HashMap<String, u64>,
) {
    let mut date = start;
    while date <= end {
        let day_dir = root
            .join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()));
        if let Ok(entries) = fs::read_dir(day_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                {
                    scan_session_file(&path, values);
                }
            }
        }
        date += ChronoDuration::days(1);
    }
}

fn scan_session_file(path: &Path, values: &mut HashMap<String, u64>) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    for line in content.lines() {
        if !line.contains("token_count") {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(day) = json
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|timestamp| timestamp.get(..10))
        else {
            continue;
        };
        let tokens = token_count_from_event(&json);
        if tokens > 0 {
            *values.entry(day.to_string()).or_default() += tokens;
        }
    }
}

fn token_count_from_event(json: &Value) -> u64 {
    let payload = json
        .get("payload")
        .filter(|payload| payload.get("type").and_then(Value::as_str) == Some("token_count"))
        .or_else(|| {
            json.get("event_msg")
                .filter(|event| event.get("type").and_then(Value::as_str) == Some("token_count"))
        });
    let Some(payload) = payload else {
        return 0;
    };
    let usage = payload
        .get("info")
        .and_then(|info| info.get("last_token_usage"))
        .or_else(|| {
            payload
                .get("info")
                .and_then(|info| info.get("total_token_usage"))
        })
        .unwrap_or(payload);
    (token_i64(usage, "input_tokens")
        + token_i64(usage, "cached_input_tokens")
        + token_i64(usage, "cache_read_input_tokens")
        + token_i64(usage, "output_tokens"))
    .max(0) as u64
}

fn heat_intensity(value: u64, max: u64) -> u8 {
    if value == 0 || max == 0 {
        0
    } else {
        ((value as f64 / max as f64) * 4.0).ceil().clamp(1.0, 4.0) as u8
    }
}

fn codex_sessions_root() -> Option<PathBuf> {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        let trimmed = codex_home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("sessions"));
        }
    }
    dirs::home_dir().map(|home| home.join(".codex").join("sessions"))
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<AppSettings, String> {
    let mut settings = app
        .state::<AppState>()
        .cache
        .lock()
        .map_err(|_| "缓存锁定失败".to_string())?
        .settings
        .clone();
    settings.autostart = autostart_enabled();
    Ok(settings)
}

#[tauri::command]
fn update_settings(app: AppHandle, settings: SettingsPatch) -> Result<AppSettings, String> {
    let state = app.state::<AppState>();
    let mut cache = state.cache.lock().map_err(|_| "缓存锁定失败".to_string())?;
    if let Some(value) = settings.refresh_interval_secs {
        cache.settings.refresh_interval_secs = value.clamp(60, 3600);
    }
    if let Some(value) = settings.privacy_mode {
        cache.settings.privacy_mode = value;
    }
    if let Some(value) = settings.hide_account {
        cache.settings.hide_account = value;
    }
    if let Some(value) = settings.autostart {
        set_autostart(value)?;
    }
    cache.settings.autostart = autostart_enabled();
    save_cache(&cache);
    Ok(cache.settings.clone())
}

#[cfg(windows)]
fn toggle_autostart_from_menu(app: &AppHandle) -> Result<(), String> {
    let next = !autostart_enabled();
    set_autostart(next)?;
    if let Ok(mut cache) = app.state::<AppState>().cache.lock() {
        cache.settings.autostart = next;
        save_cache(&cache);
    }
    Ok(())
}

#[cfg(windows)]
fn autostart_enabled() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .ok()
        .and_then(|key| key.get_value::<String, _>("CodexUsageTray").ok())
        .is_some()
}

#[cfg(not(windows))]
fn autostart_enabled() -> bool {
    false
}

#[cfg(windows)]
fn set_autostart(enabled: bool) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map_err(|e| format!("打开开机启动注册表失败: {e}"))?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败: {e}"))?;
        key.set_value("CodexUsageTray", &format!("\"{}\"", exe.display()))
            .map_err(|e| format!("写入开机启动失败: {e}"))?;
    } else {
        let _ = key.delete_value("CodexUsageTray");
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_autostart(_enabled: bool) -> Result<(), String> {
    Ok(())
}

fn cache_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("CodexUsageTray")
        .join("cache.json")
}

fn load_cache() -> AppCache {
    fs::read_to_string(cache_path())
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &AppCache) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(path, json);
    }
}

fn json_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

fn parse_datetime(input: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(input)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn token_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn format_reset_countdown(reset_at: DateTime<Utc>) -> String {
    let diff = reset_at - Utc::now();
    if diff.num_minutes() <= 0 {
        return "now".to_string();
    }
    let hours = diff.num_hours();
    let mins = diff.num_minutes() % 60;
    if hours >= 24 {
        let days = hours / 24;
        let rem_hours = hours % 24;
        if rem_hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {rem_hours}h")
        }
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn plan_label(plan: &str) -> String {
    match plan {
        "free" => "ChatGPT Free",
        "go" => "Codex Go",
        "plus" => "ChatGPT Plus",
        "pro" => "ChatGPT Pro",
        "team" => "ChatGPT Team",
        "business" => "ChatGPT Business",
        "enterprise" => "ChatGPT Enterprise",
        "education" | "edu" => "ChatGPT Education",
        other => other,
    }
    .to_string()
}

fn slugify(input: &str) -> String {
    let mut output = String::new();
    let mut dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash && !output.is_empty() {
            output.push('-');
            dash = true;
        }
    }
    output.trim_matches('-').to_string()
}

fn titleize(input: &str) -> String {
    input
        .split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_usage_windows_and_spark_limits() {
        let usage = build_usage_snapshot(
            &json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": {"used_percent": 75, "limit_window_seconds": 18000},
                    "secondary_window": {"used_percent": 59, "limit_window_seconds": 604800}
                },
                "additional_rate_limits": [{
                    "limit_name": "Codex Spark",
                    "metered_feature": "codex_spark",
                    "rate_limit": {"primary_window": {"used_percent": 98, "limit_window_seconds": 18000}}
                }],
                "credits": {"has_credits": true, "unlimited": false, "balance": 12.7}
            }),
            None,
        );
        assert_eq!(usage.primary.used_percent, 75.0);
        assert_eq!(usage.secondary.unwrap().window_minutes, Some(10080));
        assert_eq!(usage.extra_rate_windows[0].id, "codex-spark");
        assert_eq!(usage.credits.unwrap().balance, 12.7);
    }

    #[test]
    fn parses_app_server_camel_case_rate_limits() {
        let usage = build_usage_snapshot_from_app_server(
            Some(&json!({"email": "u@example.com", "planType": "plus"})),
            &json!({
                "rateLimits": {
                    "primary": {"usedPercent": 44, "windowDurationMins": 300, "resetsAt": "2026-06-16T12:00:00Z"},
                    "secondary": {"usedPercent": 12, "windowDurationMins": 10080}
                },
                "rateLimitsByLimitId": {
                    "codex_spark": {
                        "limitName": "Codex Spark",
                        "primary": {"usedPercent": 8, "windowDurationMins": 300}
                    }
                }
            }),
            None,
        );
        assert_eq!(usage.account_email.as_deref(), Some("u@example.com"));
        assert_eq!(usage.primary.used_percent, 44.0);
        assert_eq!(usage.secondary.unwrap().window_minutes, Some(10080));
        assert_eq!(usage.extra_rate_windows[0].title, "Codex Spark");
        assert_eq!(usage.source, "app-server");
    }

    #[test]
    fn token_count_parser_ignores_prompt_content() {
        let event = json!({
            "timestamp": "2026-06-16T10:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"last_token_usage": {"input_tokens": 10, "cached_input_tokens": 3, "output_tokens": 4}}
            },
            "message": "this prompt text is ignored"
        });
        assert_eq!(token_count_from_event(&event), 17);
    }
}
