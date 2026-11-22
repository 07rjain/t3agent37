//! The Tauri desktop tray app for t3agent37: tray icon, popover window, and UI commands.

use crate::agent37::{self, Agent37, InstanceType, Region};
use crate::app::{self, App, Costs, MachineState, MachineView, Setup};
use crate::machine::provision::{SizeInfo, SIZES};
use crate::machine::Event;
use crate::mcp;
use crate::store::Settings;
use crate::updater::{self, InstallOutcome, UpdateInfo, UpdateStatus};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_notification::NotificationExt;

const PANEL: &str = "panel";
const COSTS_EVERY: Duration = Duration::from_secs(180);
const UPDATE_EVERY: Duration = Duration::from_secs(4 * 3600);
const TRAY_ICON: &[u8] = if cfg!(target_os = "macos") {
    include_bytes!("../icons/tray@2x.png")
} else {
    include_bytes!("../icons/64x64.png")
};

#[derive(Default)]
struct Ui {
    app: Mutex<Option<Arc<App>>>,
    setup: Mutex<SetupState>,
    account: Mutex<Option<Account>>,
    mcp: Mutex<McpStatus>,
    update: Mutex<UpdateInfo>,
    update_notified_version: Mutex<Option<String>>,
    update_menu_item: Mutex<Option<MenuItem<tauri::Wry>>>,
    windowed: AtomicBool,
    init_lock: tokio::sync::Mutex<()>,
    update_lock: tokio::sync::Mutex<()>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpStatus {
    claude_code: Option<bool>,
    codex: Option<bool>,
}

async fn mcp_status() -> McpStatus {
    let status = |client| async move {
        if mcp::installed(client).await { Some(mcp::registered(client).await) } else { None }
    };
    let (claude_code, codex) = tokio::join!(status(mcp::Client::ClaudeCode), status(mcp::Client::Codex));
    McpStatus { claude_code, codex }
}

async fn refresh_mcp(handle: &AppHandle) {
    let status = mcp_status().await;
    *handle.state::<Ui>().mcp.lock().unwrap() = status;
    emit_state(handle);
}

#[derive(Clone, Default)]
struct SetupState {
    setup: Setup,
    busy: bool,
    sign_in_code: Option<String>,
    sign_in_url: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    identifier: Option<String>,
    plan: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiState {
    setup: Setup,
    setup_busy: bool,
    sign_in_code: Option<String>,
    sign_in_url: Option<String>,
    account: Option<Account>,
    machines: Vec<MachineView>,
    costs: Costs,
    settings: Settings,
    sizes: &'static [SizeInfo],
    mcp: McpStatus,
    update: UpdateInfo,
}

impl Ui {
    fn app(&self) -> Result<Arc<App>, String> {
        self.app.lock().unwrap().clone().ok_or_else(|| "t3agent37 is not set up yet".to_owned())
    }

    fn state(&self) -> UiState {
        let setup = self.setup.lock().unwrap().clone();
        let app = self.app.lock().unwrap().clone();
        UiState {
            setup: setup.setup,
            setup_busy: setup.busy,
            sign_in_code: setup.sign_in_code,
            sign_in_url: setup.sign_in_url,
            account: self.account.lock().unwrap().clone(),
            machines: app.as_ref().map(|a| a.views()).unwrap_or_default(),
            costs: app.as_ref().map(|a| a.costs()).unwrap_or_default(),
            settings: app.as_ref().map(|a| a.settings()).unwrap_or_default(),
            sizes: SIZES,
            mcp: self.mcp.lock().unwrap().clone(),
            update: self.update.lock().unwrap().clone(),
        }
    }
}

fn emit_state(handle: &AppHandle) {
    let state = handle.state::<Ui>().state();
    update_tray(handle, &state);
    let _ = handle.emit("state", state);
}

fn update_tray(handle: &AppHandle, state: &UiState) {
    let menu_item = handle.state::<Ui>().update_menu_item.lock().unwrap().clone();
    if let Some(item) = menu_item {
        let label = match state.update.status {
            UpdateStatus::Checking => "Checking for updates…".to_owned(),
            UpdateStatus::Downloading => "Downloading update…".to_owned(),
            UpdateStatus::Installing => "Installing update…".to_owned(),
            _ if state.update.update_available => match state.update.latest_version.as_deref() {
                Some(v) => format!("Update to v{v}"),
                None => "Update t3agent37".to_owned(),
            },
            _ => format!("Check for updates (v{})…", state.update.current_version),
        };
        let _ = item.set_text(label);
    }
    let Some(tray) = handle.tray_by_id("main") else { return };
    let working = state.machines.iter().filter(|m| m.state == MachineState::Working).count();
    let awake = state
        .machines
        .iter()
        .filter(|m| matches!(m.state, MachineState::Idle | MachineState::Working | MachineState::Waking))
        .count();
    let mut tip = match (working, awake) {
        (0, 0) => "t3agent37 · all machines asleep".to_owned(),
        (0, n) => format!("t3agent37 · {n} awake"),
        (w, _) => format!("t3agent37 · {w} working"),
    };
    if state.update.update_available {
        if let Some(v) = state.update.latest_version.as_deref() {
            tip.push_str(&format!(" · v{v} available"));
        }
    }
    let _ = tray.set_tooltip(Some(tip));
}

async fn init(handle: AppHandle) {
    let ui = handle.state::<Ui>();
    let _init = ui.init_lock.lock().await;
    if ui.app.lock().unwrap().is_some() {
        return;
    }
    let data_dir = handle.path().app_data_dir().expect("app data dir");
    let (setup, a37, t3) = app::check_setup(Some(&data_dir)).await;
    ui.setup.lock().unwrap().setup = setup;
    if let Some(agent37) = &a37 {
        refresh_account(&handle, agent37).await;
    }
    if let (Setup::Ready, Some(agent37), Some(t3)) = (setup, a37, t3) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        match App::new(agent37, t3, data_dir, tx) {
            Ok(app) => {
                *ui.app.lock().unwrap() = Some(app.clone());
                if let Err(e) = apply_autostart(&handle, app.settings().start_at_login) {
                    tracing::warn!("start at login: {e}");
                }
                tauri::async_runtime::spawn(events(handle.clone(), app.clone(), rx));
                tauri::async_runtime::spawn(background(handle.clone(), app.clone()));
                let mcp_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = mcp::serve(app).await {
                        notify(&mcp_handle, "t3agent37", &format!("Coding agents cannot reach t3agent37: {e:#}"));
                    }
                });
                tauri::async_runtime::spawn({
                    let handle = handle.clone();
                    async move { refresh_mcp(&handle).await }
                });
            }
            Err(e) => notify(&handle, "t3agent37 could not start", &format!("{e:#}")),
        }
    }
    emit_state(&handle);
}

fn set_account_from_billing(handle: &AppHandle, b: &agent37::BillingInfo) {
    let ui = handle.state::<Ui>();
    *ui.account.lock().unwrap() = Some(Account {
        identifier: Some(format!("Wallet ${:.2}", b.balance_dollars)),
        plan: b.tier.clone().or_else(|| Some(format!("{} GB RAM limit", b.memory_limit_gb))),
    });
}

async fn refresh_account(handle: &AppHandle, agent37: &Agent37) {
    if let Ok(b) = agent37.billing().await {
        set_account_from_billing(handle, &b);
    }
}

async fn events(handle: AppHandle, app: Arc<App>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Persist => app.save(),
            Event::Changed => emit_state(&handle),
            Event::Notify { title, body } => notify(&handle, &title, &body),
            Event::SignInCode(code) => {
                handle.state::<Ui>().setup.lock().unwrap().sign_in_code = code;
                emit_state(&handle);
            }
            Event::SignInUrl(url) => {
                handle.state::<Ui>().setup.lock().unwrap().sign_in_url = url;
                emit_state(&handle);
            }
        }
    }
}

async fn background(handle: AppHandle, app: Arc<App>) {
    loop {
        app.refresh_costs().await;
        if let Some(b) = app.billing() {
            set_account_from_billing(&handle, &b);
        }
        emit_state(&handle);
        tokio::time::sleep(COSTS_EVERY).await;
    }
}

async fn perform_update_check(handle: &AppHandle, interactive: bool) -> Result<UpdateInfo, String> {
    let ui = handle.state::<Ui>();
    let _guard = ui.update_lock.lock().await;
    {
        let mut u = ui.update.lock().unwrap();
        u.status = UpdateStatus::Checking;
        u.error = None;
    }
    emit_state(handle);
    match updater::check_latest().await {
        Ok(info) => {
            let should_notify = info.update_available
                && info.latest_version.as_ref().is_some_and(|v| {
                    let mut last = ui.update_notified_version.lock().unwrap();
                    if last.as_deref() == Some(v.as_str()) {
                        false
                    } else {
                        *last = Some(v.clone());
                        true
                    }
                });
            *ui.update.lock().unwrap() = info.clone();
            emit_state(handle);
            if should_notify {
                if let Some(v) = info.latest_version.as_deref() {
                    notify(
                        handle,
                        &format!("t3agent37 v{v} available"),
                        "Open the menubar app and click Update to install the latest release.",
                    );
                }
            }
            Ok(info)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::warn!("update check failed: {msg}");
            if interactive {
                let mut u = ui.update.lock().unwrap();
                u.status = UpdateStatus::Error;
                u.error = Some(msg.clone());
                drop(u);
                emit_state(handle);
            } else {
                let mut u = ui.update.lock().unwrap();
                if u.status == UpdateStatus::Checking {
                    u.status = UpdateStatus::Idle;
                }
                drop(u);
                emit_state(handle);
            }
            Err(msg)
        }
    }
}

async fn perform_install_update(handle: &AppHandle) -> Res {
    let ui = handle.state::<Ui>();
    let _guard = ui.update_lock.lock().await;
    let mut info = ui.update.lock().unwrap().clone();
    if !info.update_available || info.asset_url.is_none() {
        {
            let mut u = ui.update.lock().unwrap();
            u.status = UpdateStatus::Checking;
            u.error = None;
        }
        emit_state(handle);
        match updater::check_latest().await {
            Ok(fresh) => {
                *ui.update.lock().unwrap() = fresh.clone();
                emit_state(handle);
                info = fresh;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let mut u = ui.update.lock().unwrap();
                u.status = UpdateStatus::Error;
                u.error = Some(msg.clone());
                drop(u);
                emit_state(handle);
                return Err(msg);
            }
        }
    }
    if !info.update_available {
        return Ok(());
    }
    let stage_handle = handle.clone();
    let res = updater::download_and_install(&info, move |stage| {
        {
            let ui = stage_handle.state::<Ui>();
            let mut u = ui.update.lock().unwrap();
            u.status = stage;
            u.error = None;
        }
        emit_state(&stage_handle);
    })
    .await;

    match res {
        Ok(InstallOutcome::RestartRequired) => {
            handle.exit(0);
            Ok(())
        }
        Ok(InstallOutcome::OpenedExternal(dest)) => {
            {
                let mut u = ui.update.lock().unwrap();
                u.status = UpdateStatus::UpToDate;
                u.update_available = false;
            }
            emit_state(handle);
            notify(
                handle,
                "t3agent37 updated",
                &format!("Installed the new version to {} and launched it.", dest.display()),
            );
            Ok(())
        }
        Ok(InstallOutcome::OpenedReleasePage(_)) => {
            {
                let mut u = ui.update.lock().unwrap();
                u.status = UpdateStatus::Available;
            }
            emit_state(handle);
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::error!("installing update failed: {msg}");
            {
                let mut u = ui.update.lock().unwrap();
                u.status = UpdateStatus::Error;
                u.error = Some(msg.clone());
            }
            emit_state(handle);
            Err(msg)
        }
    }
}

async fn background_updates(handle: AppHandle) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    loop {
        let _ = perform_update_check(&handle, false).await;
        tokio::time::sleep(UPDATE_EVERY).await;
    }
}

fn notify(handle: &AppHandle, title: &str, body: &str) {
    let _ = handle.notification().builder().title(title).body(body).show();
}

fn apply_autostart(handle: &AppHandle, on: bool) -> Result<(), tauri_plugin_autostart::Error> {
    let autolaunch = handle.autolaunch();
    if on { autolaunch.enable() } else { autolaunch.disable() }
}

struct Anchor {
    icon: tauri::Rect,
    cursor: PhysicalPosition<f64>,
}

fn toggle_panel(handle: &AppHandle, anchor: Option<Anchor>) {
    let Some(panel) = handle.get_webview_window(PANEL) else { return };
    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
    } else {
        open_panel(handle, anchor);
    }
}

fn windowed(handle: &AppHandle) -> bool {
    handle.state::<Ui>().windowed.load(Ordering::Relaxed)
}

fn open_panel(handle: &AppHandle, anchor: Option<Anchor>) {
    let Some(panel) = handle.get_webview_window(PANEL) else { return };
    if !windowed(handle) {
        place_under_tray(handle, &panel, anchor);
    }
    show_focused(&panel);
    emit_state(handle);
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move { refresh_mcp(&handle).await });
}

fn place_under_tray(handle: &AppHandle, panel: &tauri::WebviewWindow, anchor: Option<Anchor>) {
    let anchor = anchor.or_else(|| {
        let cursor = handle.cursor_position().ok()?;
        let icon = handle.tray_by_id("main")?.rect().ok().flatten().unwrap_or(tauri::Rect {
            position: cursor.into(),
            size: tauri::PhysicalSize::new(0, 0).into(),
        });
        Some(Anchor { icon, cursor })
    });
    if let Some(at) = anchor.and_then(|a| panel_position(handle, panel, &a)) {
        let _ = panel.set_position(at);
    }
}

#[cfg(target_os = "linux")]
fn follow_tray_host(handle: &AppHandle) {
    use gtk::gio;
    let (appeared, vanished) = (handle.clone(), handle.clone());
    gio::bus_watch_name(
        gio::BusType::Session,
        "org.kde.StatusNotifierWatcher",
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| set_windowed(&appeared, false),
        move |_, _| set_windowed(&vanished, true),
    );
}

#[cfg(target_os = "linux")]
fn set_windowed(handle: &AppHandle, windowed: bool) {
    if handle.state::<Ui>().windowed.swap(windowed, Ordering::Relaxed) == windowed {
        return;
    }
    let Some(panel) = handle.get_webview_window(PANEL) else { return };
    let _ = panel.set_decorations(windowed);
    let _ = panel.set_skip_taskbar(!windowed);
    let _ = panel.set_always_on_top(!windowed);
    if windowed {
        if let Ok(window) = panel.gtk_window() {
            gtk::prelude::GtkWindowExt::set_position(&window, gtk::WindowPosition::Center);
        }
        open_panel(handle, None);
    } else {
        let _ = panel.hide();
    }
}

fn show_focused(panel: &tauri::WebviewWindow) {
    let _ = panel.show();
    #[cfg(target_os = "linux")]
    {
        let panel = panel.clone();
        let _ = panel.clone().run_on_main_thread(move || {
            if let Ok(window) = panel.gtk_window() {
                present(&window);
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = panel.set_focus();
}

#[cfg(target_os = "linux")]
fn present(window: &gtk::ApplicationWindow) {
    use gtk::prelude::*;
    window.realize();
    match window.window().and_then(|w| w.downcast::<gdkx11::X11Window>().ok()) {
        Some(w) => window.present_with_time(gdkx11::functions::x11_get_server_time(&w)),
        None => window.present(),
    }
}

fn panel_position(handle: &AppHandle, panel: &tauri::WebviewWindow, a: &Anchor) -> Option<tauri::Position> {
    const GAP: f64 = 6.0;
    let monitors = handle.available_monitors().ok()?;
    let unit = |m: &tauri::Monitor| if cfg!(target_os = "macos") { m.scale_factor() } else { 1.0 };
    let icon_in = |m: &tauri::Monitor| {
        let (at, size) = (a.icon.position.to_physical::<f64>(m.scale_factor()), a.icon.size.to_physical::<f64>(m.scale_factor()));
        let u = unit(m);
        (at.x / u, at.y / u, size.width / u, size.height / u)
    };
    let bounds = |m: &tauri::Monitor| {
        let u = unit(m);
        let (p, s) = (m.position(), m.size());
        (f64::from(p.x) / u, f64::from(p.y) / u, f64::from(s.width) / u, f64::from(s.height) / u)
    };
    let contains = |m: &tauri::Monitor| {
        let (x, y, ..) = icon_in(m);
        let (mx, my, mw, mh) = bounds(m);
        x >= mx && x < mx + mw && y >= my && y < my + mh
    };
    let monitor = if cfg!(target_os = "macos") {
        const MENU_BAR_PT: f64 = 24.0;
        monitors.iter().filter(|m| contains(m)).min_by(|x, y| {
            let off = |m: &tauri::Monitor| (icon_in(m).3 / MENU_BAR_PT).ln().abs();
            off(x).total_cmp(&off(y))
        })
    } else {
        monitors.iter().find(|m| {
            let (mx, my, mw, mh) = bounds(m);
            a.cursor.x >= mx && a.cursor.x < mx + mw && a.cursor.y >= my && a.cursor.y < my + mh
        })
    }?;
    let (ix, iy, iw, ih) = icon_in(monitor);
    let (mx, my, mw, mh) = bounds(monitor);
    let logical = panel.outer_size().ok()?.to_logical::<f64>(panel.scale_factor().ok()?);
    let (w, h) = if cfg!(target_os = "macos") {
        (logical.width, logical.height)
    } else {
        (logical.width * monitor.scale_factor(), logical.height * monitor.scale_factor())
    };
    let gap = GAP * if cfg!(target_os = "macos") { 1.0 } else { monitor.scale_factor() };
    let x = (ix + iw / 2.0 - w / 2.0).clamp(mx + gap, (mx + mw - w - gap).max(mx + gap));
    let y = if iy < my + mh / 2.0 { iy + ih + gap } else { iy - h - gap };
    Some(if cfg!(target_os = "macos") {
        tauri::Position::Logical(tauri::LogicalPosition::new(x, y))
    } else {
        tauri::Position::Physical(tauri::PhysicalPosition::new(x.round() as i32, y.round() as i32))
    })
}

type Res<T = ()> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
fn get_state(ui: State<'_, Ui>) -> UiState {
    ui.state()
}

#[tauri::command]
async fn save_api_key(handle: AppHandle, api_key: String) -> Res {
    let client = Agent37::from_key_with_env(api_key.clone()).map_err(err)?;
    client.billing().await.map_err(|e| format!("Agent37 rejected that API key: {e:#}"))?;
    let data_dir = handle.path().app_data_dir().expect("app data dir");
    agent37::save_api_key(&data_dir, &api_key).map_err(err)?;
    init(handle).await;
    Ok(())
}

#[tauri::command]
async fn create_machine(
    ui: State<'_, Ui>,
    name: String,
    size: String,
    instance_type: Option<InstanceType>,
    region: Option<Region>,
    disk_gb: Option<u32>,
    projects: Option<Vec<String>>,
) -> Res {
    ui.app()?
        .create_machine(
            &name,
            &size,
            instance_type.unwrap_or_default(),
            region.unwrap_or_default(),
            disk_gb,
            projects.unwrap_or_default(),
        )
        .await
        .map(drop)
        .map_err(err)
}

#[tauri::command]
async fn local_projects(ui: State<'_, Ui>) -> Res<Vec<crate::projects::Project>> {
    ui.app()?.local_projects().await.map_err(err)
}

#[tauri::command]
async fn add_project(ui: State<'_, Ui>, id: String, project_id: String) -> Res {
    let app = ui.app()?;
    let machine = app.machine(&id).map_err(err)?;
    app.add_project(&machine, &project_id).await.map(drop).map_err(err)
}

#[tauri::command]
async fn wake_machine(ui: State<'_, Ui>, id: String) -> Res {
    ui.app()?.machine(&id).map_err(err)?.ensure_awake().await.map_err(err)
}

#[tauri::command]
async fn sleep_machine(ui: State<'_, Ui>, id: String) -> Res {
    ui.app()?.machine(&id).map_err(err)?.sleep_now().await.map_err(err)
}

#[tauri::command]
async fn open_machine(ui: State<'_, Ui>, id: String, thread: Option<String>) -> Res {
    let app = ui.app()?;
    let machine = app.machine(&id).map_err(err)?;
    app.open_in_t3(&machine, thread.as_deref()).map_err(err)
}

#[tauri::command]
async fn pair_machine(ui: State<'_, Ui>, id: String) -> Res {
    let app = ui.app()?;
    let machine = app.machine(&id).map_err(err)?;
    app.pair(&machine).map_err(err)
}

#[tauri::command]
async fn latest_thread(ui: State<'_, Ui>, id: String) -> Res<Option<String>> {
    Ok(ui.app()?.machine(&id).map_err(err)?.latest_thread().await)
}

#[tauri::command]
fn get_machine_details(ui: State<'_, Ui>, id: String) -> Res<app::MachineDetails> {
    ui.app()?.details(&id).map_err(err)
}

#[tauri::command]
async fn sign_in_agent(ui: State<'_, Ui>, id: String, agent: String) -> Res {
    let agent = crate::machine::signin::Agent::from_name(&agent)
        .ok_or_else(|| format!("{agent} has to be signed in from T3 Code's settings"))?;
    ui.app()?.machine(&id).map_err(err)?.sign_in(agent).await.map_err(err)
}

#[tauri::command]
async fn sign_in_github(ui: State<'_, Ui>, id: String, token: Option<String>) -> Res {
    ui.app()?.machine(&id).map_err(err)?.sign_in_github(token.as_deref()).await.map_err(err)
}

#[tauri::command]
async fn turn_on_connect(ui: State<'_, Ui>, id: String) -> Res {
    ui.app()?.machine(&id).map_err(err)?.turn_on_connect().await.map_err(err)
}

#[tauri::command]
async fn keep_awake(ui: State<'_, Ui>, id: String, minutes: Option<u32>) -> Res {
    ui.app()?.machine(&id).map_err(err)?.keep_awake(minutes).await.map_err(err)
}

#[tauri::command]
async fn resize_machine(ui: State<'_, Ui>, id: String, size: Option<String>, disk_gb: Option<u32>) -> Res {
    ui.app()?.machine(&id).map_err(err)?.resize(size.as_deref(), disk_gb).await.map_err(err)
}

#[tauri::command]
async fn delete_machine(ui: State<'_, Ui>, id: String, discard: Option<bool>) -> Res {
    let app = ui.app()?;
    let machine = app.machine(&id).map_err(err)?;
    app.delete_machine(&machine, discard.unwrap_or(false)).await.map_err(err)
}

#[tauri::command]
async fn update_settings(handle: AppHandle, ui: State<'_, Ui>, settings: Settings) -> Res {
    let app = ui.app()?;
    if app.settings().start_at_login != settings.start_at_login {
        apply_autostart(&handle, settings.start_at_login).map_err(|e| format!("Couldn't change start at login: {e}"))?;
    }
    app.update_settings(settings);
    emit_state(&handle);
    Ok(())
}

#[tauri::command]
async fn setup_action(handle: AppHandle, ui: State<'_, Ui>, action: String) -> Res {
    match action.as_str() {
        "openApiKeys" => app::open_url("https://www.agent37.com/dashboard/cloud/api-keys").map_err(err)?,
        "installT3" => app::open_url("https://t3.codes").map_err(err)?,
        "recheck" => {}
        other => return Err(format!("unknown setup action {other}")),
    }
    if ui.app.lock().unwrap().is_none() {
        init(handle.clone()).await;
    } else if let Ok(app) = ui.app() {
        refresh_account(&handle, &app.shared.agent37).await;
        emit_state(&handle);
    }
    Ok(())
}

#[tauri::command]
async fn add_mcp(handle: AppHandle, client: mcp::Client) -> Res {
    let result = mcp::register(client).await.map_err(err);
    refresh_mcp(&handle).await;
    result
}

#[tauri::command]
async fn check_for_updates(handle: AppHandle) -> Res<UpdateInfo> {
    perform_update_check(&handle, true).await
}

#[tauri::command]
async fn install_update(handle: AppHandle) -> Res {
    perform_install_update(&handle).await
}

#[tauri::command]
fn open_release_notes(ui: State<'_, Ui>) -> Res {
    let url = ui
        .update
        .lock()
        .unwrap()
        .release_url
        .clone()
        .unwrap_or_else(updater::releases_page_url);
    app::open_url(&url).map_err(err)
}

#[tauri::command]
fn quit_app(handle: AppHandle) {
    handle.exit(0);
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|handle, _, _| open_panel(handle, None)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_notification::init())
        .manage(Ui::default())
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_api_key,
            create_machine,
            local_projects,
            add_project,
            wake_machine,
            sleep_machine,
            open_machine,
            pair_machine,
            delete_machine,
            resize_machine,
            keep_awake,
            sign_in_agent,
            sign_in_github,
            turn_on_connect,
            get_machine_details,
            latest_thread,
            update_settings,
            setup_action,
            add_mcp,
            check_for_updates,
            install_update,
            open_release_notes,
            quit_app
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let open = MenuItem::with_id(app, "open", "Open t3agent37", true, None::<&str>)?;
            let update_item = MenuItem::with_id(
                app,
                "update",
                format!("Check for updates (v{})…", updater::CURRENT_VERSION),
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Quit t3agent37", true, None::<&str>)?;
            *app.state::<Ui>().update_menu_item.lock().unwrap() = Some(update_item.clone());
            let menu = Menu::with_items(app, &[&open, &update_item, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(tauri::image::Image::from_bytes(TRAY_ICON)?)
                .icon_as_template(true)
                .tooltip("t3agent37")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => toggle_panel(app, None),
                    "update" => {
                        let handle = app.clone();
                        let already_available = handle.state::<Ui>().update.lock().unwrap().update_available;
                        open_panel(&handle, None);
                        tauri::async_runtime::spawn(async move {
                            if already_available {
                                if let Err(e) = perform_install_update(&handle).await {
                                    notify(&handle, "Update failed", &e);
                                }
                            } else {
                                match perform_update_check(&handle, true).await {
                                    Ok(info) if info.update_available => {
                                        if let Some(v) = info.latest_version.as_deref() {
                                            notify(
                                                &handle,
                                                &format!("t3agent37 v{v} available"),
                                                "Click Update in the menubar panel to install.",
                                            );
                                        }
                                    }
                                    Ok(info) => {
                                        notify(
                                            &handle,
                                            "t3agent37 is up to date",
                                            &format!("You are running v{}.", info.current_version),
                                        );
                                    }
                                    Err(e) => notify(&handle, "Update check failed", &e),
                                }
                            }
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        position,
                        ..
                    } = event
                    {
                        toggle_panel(tray.app_handle(), Some(Anchor { icon: rect, cursor: position }));
                    }
                })
                .build(app)?;

            if let Some(panel) = app.get_webview_window(PANEL) {
                let hide = panel.clone();
                let handle = app.handle().clone();
                let keep_open = std::env::var_os("T3AGENT37_KEEP_PANEL_OPEN").is_some();
                panel.on_window_event(move |e| match e {
                    WindowEvent::Focused(false) if !keep_open && !windowed(&handle) => {
                        let _ = hide.hide();
                    }
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = hide.hide();
                    }
                    _ => {}
                });
            }
            #[cfg(target_os = "linux")]
            follow_tray_host(app.handle());
            tauri::async_runtime::spawn(init(app.handle().clone()));
            tauri::async_runtime::spawn(background_updates(app.handle().clone()));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building t3agent37")
        .run(|_, event| {
            if let tauri::RunEvent::ExitRequested { api, code: None, .. } = event {
                api.prevent_exit();
            }
        });
}
