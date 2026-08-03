use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        Mutex,
    },
    thread,
};

#[cfg(target_os = "windows")]
use std::{os::windows::process::CommandExt, process::Command};

use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, Wry,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use xmic_engine_protocol::{
    AecPreset, ConfigurationIssue, EngineSettings, EngineStatus, UnprocessedOutput,
    VirtualDeviceIssueReason,
};
use xmic_platform_api::EndpointDescriptor;
use xmic_platform_windows::start_system_lifecycle_monitor;

mod device_profiles;
mod endpoint_cache;
mod engine;
mod engine_client;
mod engine_ipc;
mod engine_process;
mod lifecycle;
mod route_validation;
mod settings_store;

use endpoint_cache::EndpointCache;
use engine::DspFeature;
use engine_client::{EngineController, TrayState};
use lifecycle::{
    BackgroundLifecycle, LifecycleController, LifecyclePreferences, LifecyclePreferencesStore,
    LifecycleStatus,
};

pub fn run_audio_engine_process() -> Result<(), String> {
    engine_process::run_from_environment()
}

const AUTOSTART_ARGUMENT: &str = "--autostart";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
const VB_CABLE_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";
const TRAY_ID: &str = "main-tray";
const MENU_SHOW: &str = "show";
const MENU_TOGGLE_PROCESSING: &str = "toggle-processing";
const MENU_TOGGLE_AEC: &str = "toggle-aec";
const MENU_TOGGLE_AGC2: &str = "toggle-agc2";
const MENU_TOGGLE_VAD_GATE: &str = "toggle-vad-gate";
const MENU_PRESET_NATURAL: &str = "preset-natural";
const MENU_PRESET_BALANCED: &str = "preset-balanced";
const MENU_PRESET_STRONG: &str = "preset-strong";
const MENU_QUIT: &str = "quit";
/// Windows hides the window with `ShowWindow(SW_HIDE)` when it goes to the tray, and WebView2 does
/// not report that as `document.hidden` — the page keeps running, invisible, and would keep polling
/// forever. Rust performs both transitions, so it is the one that can say which happened.
const WINDOW_VISIBILITY_EVENT: &str = "window-visibility";
const LANGUAGE_ZH_CN: u8 = 0;
const LANGUAGE_EN: u8 = 1;

#[derive(Clone, Copy)]
enum UiLanguage {
    ZhCn,
    En,
}

impl UiLanguage {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "zh-CN" => Some(Self::ZhCn),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::ZhCn => LANGUAGE_ZH_CN,
            Self::En => LANGUAGE_EN,
        }
    }

    fn from_code(value: u8) -> Self {
        if value == LANGUAGE_EN {
            Self::En
        } else {
            Self::ZhCn
        }
    }
}

struct TrayLabels {
    show: &'static str,
    start_processing: &'static str,
    stop_processing: &'static str,
    cancel_pending: &'static str,
    configure_microphone: &'static str,
    preset: &'static str,
    quit: &'static str,
    processing_tooltip: &'static str,
    unprocessed_raw_tooltip: &'static str,
    unprocessed_silent_tooltip: &'static str,
    pending_tooltip: &'static str,
    configuration_tooltip: &'static str,
}

fn tray_labels(language: UiLanguage) -> TrayLabels {
    match language {
        UiLanguage::ZhCn => TrayLabels {
            show: "打开 XMic",
            start_processing: "启动处理",
            stop_processing: "停止处理",
            cancel_pending: "取消自动启动",
            configure_microphone: "配置音频设备",
            preset: "处理强度",
            quit: "退出 XMic",
            processing_tooltip: "XMic - 处理中",
            unprocessed_raw_tooltip: "XMic - 未处理 · 原始麦克风",
            unprocessed_silent_tooltip: "XMic - 未处理 · 静音",
            pending_tooltip: "XMic - 等待恢复处理",
            configuration_tooltip: "XMic - 需要配置音频设备",
        },
        UiLanguage::En => TrayLabels {
            show: "Open XMic",
            start_processing: "Start processing",
            stop_processing: "Stop processing",
            cancel_pending: "Cancel automatic start",
            configure_microphone: "Configure audio devices",
            preset: "Processing strength",
            quit: "Quit XMic",
            processing_tooltip: "XMic - Processing",
            unprocessed_raw_tooltip: "XMic - Not processing · raw microphone",
            unprocessed_silent_tooltip: "XMic - Not processing · silent",
            pending_tooltip: "XMic - Waiting to resume",
            configuration_tooltip: "XMic - Audio device setup required",
        },
    }
}

struct TrayMenuState {
    language: AtomicU8,
    /// What the tray was last drawn with. The UI polls about once a second and the tray is almost
    /// always identical between polls, so this is what stops every poll from rewriting ten menu
    /// items and cloning the whole RGBA icon on the main thread.
    rendered: Mutex<Option<RenderedTray>>,
    show: MenuItem<Wry>,
    toggle_processing: MenuItem<Wry>,
    toggle_aec: CheckMenuItem<Wry>,
    preset_menu: Submenu<Wry>,
    preset_natural: CheckMenuItem<Wry>,
    preset_balanced: CheckMenuItem<Wry>,
    preset_strong: CheckMenuItem<Wry>,
    toggle_agc2: CheckMenuItem<Wry>,
    toggle_vad_gate: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Every value `refresh_tray` actually paints. The menu labels not listed here are derived from
/// `language`, so comparing the code covers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderedTray {
    language: u8,
    label: &'static str,
    enabled: bool,
    tooltip: &'static str,
    aec_enabled: bool,
    preset: AecPreset,
    agc2_enabled: bool,
    vad_gate_enabled: bool,
    processing: bool,
}

/// The green microphone and its grey twin. The icon carries almost all of its meaning in colour, and
/// the tray renders it around 16 px, where a corner badge would be a handful of muddy pixels — so the
/// "not processing" variant is the same shape with the colour taken out, computed from the real icon
/// instead of shipping a second asset that could drift from it.
struct TrayIcons {
    processing: Image<'static>,
    unprocessed: Image<'static>,
}

impl TrayIcons {
    fn from_window_icon(icon: &Image<'_>) -> Self {
        let processing = Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height());
        Self {
            unprocessed: Image::new_owned(
                desaturate_rgba(icon.rgba()),
                icon.width(),
                icon.height(),
            ),
            processing,
        }
    }

    fn select(&self, processing: bool) -> &Image<'static> {
        if processing {
            &self.processing
        } else {
            &self.unprocessed
        }
    }
}

/// Rec. 709 luminance, alpha untouched, so the dark background stays dark and the coloured microphone
/// turns grey.
fn desaturate_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut output = rgba.to_vec();
    for pixel in output.chunks_exact_mut(4) {
        let luminance = (0.2126 * f32::from(pixel[0])
            + 0.7152 * f32::from(pixel[1])
            + 0.0722 * f32::from(pixel[2]))
        .round()
        .clamp(0.0, 255.0) as u8;
        pixel[0] = luminance;
        pixel[1] = luminance;
        pixel[2] = luminance;
    }
    output
}

#[tauri::command]
fn get_bootstrap_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    get_engine_status(app, controller)
}

#[tauri::command]
fn list_audio_endpoints(
    cache: State<'_, EndpointCache>,
) -> Result<Vec<EndpointDescriptor>, String> {
    cache
        .endpoints()
        .map(|endpoints| endpoints.as_ref().clone())
}

#[tauri::command]
fn get_engine_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    let status = engine_status(&app, &controller)?;
    app.state::<LifecycleController>()
        .synchronize_configuration_issue(status.configuration_issue.clone(), false);
    schedule_tray_refresh(&app, TrayState::from_status(&status));
    Ok(status)
}

#[tauri::command]
fn get_lifecycle_status(
    app: AppHandle,
    lifecycle: State<'_, LifecycleController>,
) -> Result<LifecycleStatus, String> {
    current_lifecycle_status(&app, &lifecycle)
}

#[tauri::command]
fn set_ui_language(app: AppHandle, language: String) -> Result<(), String> {
    let language = UiLanguage::parse(&language)
        .ok_or_else(|| format!("unsupported interface language: {language}"))?;
    let menu = app
        .try_state::<TrayMenuState>()
        .ok_or_else(|| "tray menu is unavailable".to_owned())?;
    menu.language.store(language.code(), Ordering::Relaxed);
    schedule_tray_refresh(&app, app.state::<EngineController>().cached_tray_state());
    Ok(())
}

#[tauri::command]
fn open_vb_cable_download() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", VB_CABLE_DOWNLOAD_URL])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("open VB-CABLE download page: {error}"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("opening the VB-CABLE page is only supported on Windows".to_owned())
    }
}

#[tauri::command]
fn update_lifecycle_preferences(
    app: AppHandle,
    lifecycle: State<'_, LifecycleController>,
    preferences: LifecyclePreferences,
) -> Result<LifecycleStatus, String> {
    let autolaunch = app.autolaunch();
    let previous_launch_at_login = autolaunch
        .is_enabled()
        .map_err(|error| format!("读取开机启动状态失败：{error}"))?;
    let launch_setting_changed = previous_launch_at_login != preferences.launch_at_login;

    if launch_setting_changed {
        set_launch_at_login(&autolaunch, preferences.launch_at_login)?;
    }

    if let Err(error) = lifecycle.set_startup_preferences(
        preferences.start_in_tray,
        preferences.start_engine_on_launch,
    ) {
        if launch_setting_changed {
            if let Err(rollback_error) = set_launch_at_login(&autolaunch, previous_launch_at_login)
            {
                return Err(format!(
                    "保存启动偏好失败：{error}；回滚开机启动也失败：{rollback_error}"
                ));
            }
        }
        return Err(error);
    }

    let status = current_lifecycle_status(&app, &lifecycle)?;
    app.emit("lifecycle-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(&app, app.state::<EngineController>().cached_tray_state());
    Ok(status)
}

fn set_launch_at_login(
    autolaunch: &tauri_plugin_autostart::AutoLaunchManager,
    enabled: bool,
) -> Result<(), String> {
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|error| format!("更新开机启动失败：{error}"))
}

#[tauri::command]
fn update_engine_settings(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
    settings: EngineSettings,
) -> Result<EngineStatus, String> {
    controller.update_settings(settings)?;
    let status = emit_status(&app, &controller)?;
    lifecycle.synchronize_configuration_issue(status.configuration_issue.clone(), false);
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

#[tauri::command]
fn set_dsp_feature_enabled(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
    feature: DspFeature,
    enabled: bool,
) -> Result<EngineStatus, String> {
    if let Err(error) = lifecycle.set_dsp_feature_enabled(&controller, feature, enabled) {
        emit_application_state(&app);
        return Err(error);
    }
    let status = emit_status(&app, &controller)?;
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

#[tauri::command]
fn start_engine(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
) -> Result<EngineStatus, String> {
    if let Err(error) = lifecycle.start_manually(&controller) {
        emit_application_state(&app);
        return Err(error);
    }
    let status = emit_status(&app, &controller)?;
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

#[tauri::command]
fn stop_engine(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
) -> Result<EngineStatus, String> {
    lifecycle.stop_manually(&controller)?;
    let status = emit_status(&app, &controller)?;
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

fn engine_status(app: &AppHandle, controller: &EngineController) -> Result<EngineStatus, String> {
    let endpoints = app.state::<EndpointCache>().endpoints()?;
    Ok(controller.status(endpoints.as_ref().clone()))
}

fn emit_status(app: &AppHandle, controller: &EngineController) -> Result<EngineStatus, String> {
    let status = engine_status(app, controller)?;
    app.state::<LifecycleController>()
        .synchronize_configuration_issue(status.configuration_issue.clone(), false);
    app.emit("engine-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(app, TrayState::from_status(&status));
    Ok(status)
}

fn current_lifecycle_status(
    app: &AppHandle,
    lifecycle: &LifecycleController,
) -> Result<LifecycleStatus, String> {
    let launch_at_login = app
        .autolaunch()
        .is_enabled()
        .map_err(|error| format!("读取开机启动状态失败：{error}"))?;
    Ok(lifecycle.status(launch_at_login))
}

fn emit_lifecycle_status(app: &AppHandle) -> Result<LifecycleStatus, String> {
    let lifecycle = app.state::<LifecycleController>();
    let status = current_lifecycle_status(app, &lifecycle)?;
    app.emit("lifecycle-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(app, app.state::<EngineController>().cached_tray_state());
    Ok(status)
}

pub(crate) fn emit_application_state(app: &AppHandle) {
    let controller = app.state::<EngineController>();
    let _ = emit_status(app, &controller);
    let _ = emit_lifecycle_status(app);
    send_pending_configuration_notice(app);
}

fn send_pending_configuration_notice(app: &AppHandle) {
    let lifecycle = app.state::<LifecycleController>();
    let Some(issue) = lifecycle.take_configuration_notice() else {
        return;
    };
    let language = app
        .try_state::<TrayMenuState>()
        .map(|menu| UiLanguage::from_code(menu.language.load(Ordering::Relaxed)))
        .unwrap_or(UiLanguage::ZhCn);
    let (title, body) = match (language, issue) {
        (UiLanguage::ZhCn, ConfigurationIssue::PhysicalCaptureRequired { .. }) => (
            "XMic 需要选择物理麦克风",
            "请打开 XMic，在设备与运行中明确选择 3.5 mm 或 USB 物理麦克风。",
        ),
        (UiLanguage::En, ConfigurationIssue::PhysicalCaptureRequired { .. }) => (
            "XMic needs a physical microphone",
            "Open XMic and explicitly select the physical 3.5 mm or USB microphone under Devices & runtime.",
        ),
        (
            UiLanguage::ZhCn,
            ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            },
        ) => (
            "XMic 未检测到虚拟麦克风",
            "请打开 XMic 安装 VB-CABLE，或配置并确认其他虚拟音频设备。",
        ),
        (UiLanguage::ZhCn, ConfigurationIssue::VirtualDeviceRequired { .. }) => (
            "XMic 需要配置虚拟麦克风",
            "请打开 XMic，完成自定义虚拟设备的写入端、读取端和安全确认。",
        ),
        (UiLanguage::En, ConfigurationIssue::VirtualDeviceRequired { .. }) => (
            "XMic needs a virtual microphone",
            "Open XMic to install VB-CABLE or finish configuring and confirming another virtual audio device.",
        ),
        (UiLanguage::ZhCn, ConfigurationIssue::VirtualRouteConflict { .. }) => (
            "XMic 音频路由冲突",
            "虚拟写入端不能同时作为实际播放设备。请打开 XMic 修正音频路由。",
        ),
        (UiLanguage::En, ConfigurationIssue::VirtualRouteConflict { .. }) => (
            "XMic audio route conflict",
            "The virtual input cannot also be the physical playback device. Open XMic to correct the route.",
        ),
    };
    let _ = app.notification().builder().title(title).body(body).show();
}

/// The one notification per application run that says the virtual microphone is not carrying processed
/// audio. It is triggered by actually hearing speech, so it only arrives when it matters — and it can
/// never arrive if the microphone is muted or unplugged in Windows, because then there is nothing to
/// hear.
pub(crate) fn send_unprocessed_speech_notice(app: &AppHandle) -> bool {
    let controller = app.state::<EngineController>();
    let Some(unprocessed_output) = controller.take_unprocessed_speech_notice() else {
        return false;
    };
    let language = app
        .try_state::<TrayMenuState>()
        .map(|menu| UiLanguage::from_code(menu.language.load(Ordering::Relaxed)))
        .unwrap_or(UiLanguage::ZhCn);
    let (title, body) = match (language, unprocessed_output) {
        (UiLanguage::ZhCn, UnprocessedOutput::RawMicrophone) => (
            "XMic 没在处理",
            "虚拟麦克风正在输出未处理的原始声音，对方可能听到回声和噪声。打开 XMic 启动处理。",
        ),
        (UiLanguage::ZhCn, UnprocessedOutput::Silence) => (
            "XMic 没在处理",
            "虚拟麦克风当前静音，对方听不到你。打开 XMic 启动处理。",
        ),
        (UiLanguage::En, UnprocessedOutput::RawMicrophone) => (
            "XMic is not processing",
            "The virtual microphone is carrying your raw audio, so others may hear echo and noise. Open XMic to start processing.",
        ),
        (UiLanguage::En, UnprocessedOutput::Silence) => (
            "XMic is not processing",
            "The virtual microphone is silent, so others cannot hear you. Open XMic to start processing.",
        ),
    };
    let _ = app.notification().builder().title(title).body(body).show();
    true
}

fn native_library_path(app: &tauri::App) -> Result<PathBuf, tauri::Error> {
    #[cfg(debug_assertions)]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("artifacts")
            .join("webrtc")
            .join("win-x64")
            .join("xmic_webrtc_dsp.dll");
        if development.is_file() {
            return Ok(development);
        }
    }

    Ok(app
        .path()
        .resource_dir()?
        .join("artifacts")
        .join("webrtc")
        .join("win-x64")
        .join("xmic_webrtc_dsp.dll"))
}

fn prepare_engine_executable(config_dir: &Path) -> std::io::Result<PathBuf> {
    let source = std::env::current_exe()?;
    let runtime_dir = config_dir
        .join("engine-runtime")
        .join(std::process::id().to_string());
    fs::create_dir_all(&runtime_dir)?;
    let executable = runtime_dir.join("xmic-audio-engine.exe");
    fs::copy(source, &executable)?;
    Ok(executable)
}

fn setup_tray(app: &mut tauri::App) -> Result<(), tauri::Error> {
    let settings = app.state::<EngineController>().settings();
    let show = MenuItem::with_id(app, MENU_SHOW, "打开 XMic", true, None::<&str>)?;
    let toggle_processing =
        MenuItem::with_id(app, MENU_TOGGLE_PROCESSING, "启动处理", true, None::<&str>)?;
    let processing_separator = PredefinedMenuItem::separator(app)?;
    let toggle_aec = CheckMenuItem::with_id(
        app,
        MENU_TOGGLE_AEC,
        "AEC",
        true,
        settings.dsp.aec.enabled,
        None::<&str>,
    )?;
    let preset_natural = CheckMenuItem::with_id(
        app,
        MENU_PRESET_NATURAL,
        "Natural",
        true,
        settings.dsp.aec.preset == AecPreset::Natural,
        None::<&str>,
    )?;
    let preset_balanced = CheckMenuItem::with_id(
        app,
        MENU_PRESET_BALANCED,
        "Balanced",
        true,
        settings.dsp.aec.preset == AecPreset::Balanced,
        None::<&str>,
    )?;
    let preset_strong = CheckMenuItem::with_id(
        app,
        MENU_PRESET_STRONG,
        "Strong",
        true,
        settings.dsp.aec.preset == AecPreset::Strong,
        None::<&str>,
    )?;
    let preset_menu = Submenu::with_items(
        app,
        "处理强度",
        settings.dsp.aec.enabled,
        &[&preset_natural, &preset_balanced, &preset_strong],
    )?;
    let toggle_agc2 = CheckMenuItem::with_id(
        app,
        MENU_TOGGLE_AGC2,
        "AGC2",
        true,
        settings.dsp.agc2.enabled,
        None::<&str>,
    )?;
    let toggle_vad_gate = CheckMenuItem::with_id(
        app,
        MENU_TOGGLE_VAD_GATE,
        "VAD Gate",
        true,
        settings.dsp.vad_gate.enabled,
        None::<&str>,
    )?;
    let quit_separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出 XMic", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &toggle_processing,
            &processing_separator,
            &toggle_aec,
            &preset_menu,
            &toggle_agc2,
            &toggle_vad_gate,
            &quit_separator,
            &quit,
        ],
    )?;

    app.manage(TrayMenuState {
        language: AtomicU8::new(LANGUAGE_ZH_CN),
        rendered: Mutex::new(None),
        show: show.clone(),
        toggle_processing: toggle_processing.clone(),
        toggle_aec: toggle_aec.clone(),
        preset_menu: preset_menu.clone(),
        preset_natural: preset_natural.clone(),
        preset_balanced: preset_balanced.clone(),
        preset_strong: preset_strong.clone(),
        toggle_agc2: toggle_agc2.clone(),
        toggle_vad_gate: toggle_vad_gate.clone(),
        quit: quit.clone(),
    });

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("XMic - 已停止")
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_TOGGLE_PROCESSING => spawn_tray_engine_toggle(app.clone()),
            MENU_TOGGLE_AEC => spawn_tray_dsp_toggle(app.clone(), DspFeature::Aec),
            MENU_TOGGLE_AGC2 => spawn_tray_dsp_toggle(app.clone(), DspFeature::Agc2),
            MENU_TOGGLE_VAD_GATE => spawn_tray_dsp_toggle(app.clone(), DspFeature::VadGate),
            MENU_PRESET_NATURAL => spawn_tray_aec_preset(app.clone(), AecPreset::Natural),
            MENU_PRESET_BALANCED => spawn_tray_aec_preset(app.clone(), AecPreset::Balanced),
            MENU_PRESET_STRONG => spawn_tray_aec_preset(app.clone(), AecPreset::Strong),
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        let icons = TrayIcons::from_window_icon(icon);
        builder = builder.icon(icons.unprocessed.clone());
        app.manage(icons);
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        emit_window_visibility(app, true);
    }
    if let Some(issue) = app
        .try_state::<LifecycleController>()
        .and_then(|lifecycle| lifecycle.configuration_issue())
    {
        let _ = app.emit("navigate-to-configuration", issue);
    }
}

fn emit_window_visibility(app: &AppHandle, visible: bool) {
    let _ = app.emit(WINDOW_VISIBILITY_EVENT, visible);
}

/// Asked once on mount, because the events alone cannot describe the state the window started in:
/// with `start_in_tray` it is never shown at all, and nothing would have emitted anything.
#[tauri::command]
fn is_main_window_visible(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        // If the window cannot say, keep polling — that is what happened before this existed.
        .unwrap_or(true)
}

fn spawn_tray_engine_toggle(app: AppHandle) {
    let _ = thread::Builder::new()
        .name("xmic-tray-action".to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
            if lifecycle.configuration_issue().is_some() {
                show_main_window(&app);
                emit_application_state(&app);
                return;
            }
            if controller.is_running() || lifecycle.has_pending_start() {
                let _ = lifecycle.stop_manually(&controller);
            } else {
                let _ = lifecycle.start_manually(&controller);
            }
            emit_application_state(&app);
        });
}

fn spawn_tray_dsp_toggle(app: AppHandle, feature: DspFeature) {
    let thread_name = match feature {
        DspFeature::Aec => "xmic-tray-aec-toggle",
        DspFeature::Rnnoise => "xmic-tray-rnnoise-toggle",
        DspFeature::Agc2 => "xmic-tray-agc2-toggle",
        DspFeature::VadGate => "xmic-tray-vad-gate-toggle",
    };
    let _ = thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
            let _ = lifecycle.toggle_dsp_feature(&controller, feature);
            emit_application_state(&app);
        });
}

fn spawn_tray_aec_preset(app: AppHandle, preset: AecPreset) {
    let thread_name = match preset {
        AecPreset::Natural => "xmic-tray-preset-natural",
        AecPreset::Balanced => "xmic-tray-preset-balanced",
        AecPreset::Strong => "xmic-tray-preset-strong",
    };
    let _ = thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
            let _ = lifecycle.set_aec_preset(&controller, preset);
            emit_application_state(&app);
        });
}

fn spawn_automatic_start(app: AppHandle) {
    let _ = thread::Builder::new()
        .name("xmic-automatic-start".to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
            let _ = lifecycle.start_automatically(&controller);
            emit_application_state(&app);
        });
}

fn schedule_tray_refresh(app: &AppHandle, tray: TrayState) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || refresh_tray(&handle, tray));
}

/// Draws the tray from a state the caller already has. Taking it as a parameter is what keeps the
/// main thread out of IPC: this runs on the main thread, and asking the helper here would freeze the
/// window for the connect timeout whenever the helper stops answering.
fn refresh_tray(app: &AppHandle, tray: TrayState) {
    let Some(menu) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let lifecycle = app.state::<LifecycleController>();
    let pending = lifecycle.has_pending_start();
    let configuration_required = lifecycle.configuration_issue().is_some();
    let available = lifecycle.is_session_available();
    let language = menu.language.load(Ordering::Relaxed);
    let labels = tray_labels(UiLanguage::from_code(language));
    let (label, enabled) = if tray.running {
        (labels.stop_processing, true)
    } else if configuration_required {
        (labels.configure_microphone, available)
    } else if pending {
        (labels.cancel_pending, true)
    } else {
        (labels.start_processing, available)
    };
    // The tooltip answers "what does the other side hear right now", which is not the same question as
    // "is a session running".
    let tooltip = if configuration_required {
        labels.configuration_tooltip
    } else if tray.processing {
        labels.processing_tooltip
    } else if pending {
        labels.pending_tooltip
    } else {
        match tray.settings.unprocessed_output {
            UnprocessedOutput::RawMicrophone => labels.unprocessed_raw_tooltip,
            UnprocessedOutput::Silence => labels.unprocessed_silent_tooltip,
        }
    };
    let rendered = RenderedTray {
        language,
        label,
        enabled,
        tooltip,
        aec_enabled: tray.settings.dsp.aec.enabled,
        preset: tray.settings.dsp.aec.preset,
        agc2_enabled: tray.settings.dsp.agc2.enabled,
        vad_gate_enabled: tray.settings.dsp.vad_gate.enabled,
        processing: tray.processing,
    };
    {
        let mut last = menu
            .rendered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == Some(rendered) {
            return;
        }
        *last = Some(rendered);
    }

    let _ = menu.show.set_text(labels.show);
    let _ = menu.toggle_processing.set_text(label);
    let _ = menu.toggle_processing.set_enabled(enabled);
    let _ = menu.toggle_aec.set_checked(rendered.aec_enabled);
    let _ = menu.preset_menu.set_text(labels.preset);
    let _ = menu.preset_menu.set_enabled(rendered.aec_enabled);
    let _ = menu
        .preset_natural
        .set_checked(rendered.preset == AecPreset::Natural);
    let _ = menu
        .preset_balanced
        .set_checked(rendered.preset == AecPreset::Balanced);
    let _ = menu
        .preset_strong
        .set_checked(rendered.preset == AecPreset::Strong);
    let _ = menu.toggle_agc2.set_checked(rendered.agc2_enabled);
    let _ = menu.toggle_vad_gate.set_checked(rendered.vad_gate_enabled);
    let _ = menu.quit.set_text(labels.quit);
    if let Some(tray_icon) = app.tray_by_id(TRAY_ID) {
        let _ = tray_icon.set_tooltip(Some(tooltip));
        if let Some(icons) = app.try_state::<TrayIcons>() {
            let _ = tray_icon.set_icon(Some(icons.select(rendered.processing).clone()));
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg(AUTOSTART_ARGUMENT)
                .build(),
        )
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|argument| argument == AUTOSTART_ARGUMENT) {
                show_main_window(app);
            }
        }))
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    emit_window_visibility(window.app_handle(), false);
                }
            }
        })
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let controller = EngineController::new(
                prepare_engine_executable(&config_dir)?,
                native_library_path(app)?,
                config_dir.clone(),
            )
            .map_err(std::io::Error::other)?;
            app.manage(controller);
            // Constructing it registers the endpoint notification client the cache invalidates
            // against, so it costs a permanent thread and a short wait here rather than on the first
            // status poll. It enumerates nothing until something asks.
            app.manage(EndpointCache::new());
            app.manage(LifecycleController::new(LifecyclePreferencesStore::new(
                config_dir.join("app-preferences.json"),
            )));
            setup_tray(app)?;
            // Grants the helper permission to drive the virtual microphone while nothing is being
            // processed; the helper starts nothing until it hears this.
            {
                let lifecycle = app.state::<LifecycleController>();
                let controller = app.state::<EngineController>();
                lifecycle.refresh_idle_permit(&controller);
            }

            match start_system_lifecycle_monitor() {
                Ok(monitor) => {
                    let background = BackgroundLifecycle::start(app.handle().clone(), monitor)
                        .map_err(std::io::Error::other)?;
                    app.manage(background);
                }
                Err(error) => {
                    let lifecycle = app.state::<LifecycleController>();
                    let controller = app.state::<EngineController>();
                    lifecycle.monitor_failed(&controller, error.to_string());
                }
            }

            if !app.state::<LifecycleController>().start_in_tray() {
                show_main_window(app.handle());
            }
            refresh_tray(
                app.handle(),
                app.state::<EngineController>().cached_tray_state(),
            );
            if app.state::<LifecycleController>().start_engine_on_launch() {
                spawn_automatic_start(app.handle().clone());
            }
            Ok(())
        });

    let app = builder
        .invoke_handler(tauri::generate_handler![
            get_bootstrap_status,
            get_engine_status,
            get_lifecycle_status,
            is_main_window_visible,
            set_ui_language,
            open_vb_cable_download,
            list_audio_endpoints,
            update_lifecycle_preferences,
            update_engine_settings,
            set_dsp_feature_enabled,
            start_engine,
            stop_engine
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app, event| {
        if matches!(event, RunEvent::ExitRequested { .. }) {
            if let Some(background) = app.try_state::<BackgroundLifecycle>() {
                background.shutdown();
            }
            let lifecycle = app.state::<LifecycleController>();
            let controller = app.state::<EngineController>();
            lifecycle.shutdown(&controller);
            controller.shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unprocessed_icon_keeps_the_shape_and_drops_the_colour() {
        // Two pixels of the real icon: the green microphone and the transparent corner.
        let green = [0x2f, 0xb5, 0x7e, 0xff];
        let transparent = [0x00, 0x00, 0x00, 0x00];
        let mut rgba = green.to_vec();
        rgba.extend_from_slice(&transparent);

        let desaturated = desaturate_rgba(&rgba);

        assert_eq!(desaturated.len(), rgba.len());
        assert_eq!(desaturated[0], desaturated[1]);
        assert_eq!(desaturated[1], desaturated[2]);
        // Alpha decides the shape, so it has to survive untouched.
        assert_eq!(desaturated[3], 0xff);
        assert_eq!(desaturated[7], 0x00);
    }
}
