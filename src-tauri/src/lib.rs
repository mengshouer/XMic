use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
    thread,
};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, Wry,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use xmic_engine_protocol::{AecPreset, EngineSettings, EngineStatus};
use xmic_platform_api::{AudioPlatform, EndpointDescriptor};
use xmic_platform_windows::{start_system_lifecycle_monitor, WasapiPlatform};

mod device_profiles;
mod engine;
mod engine_client;
mod engine_ipc;
mod engine_process;
mod lifecycle;
mod settings_store;

use engine::DspFeature;
use engine_client::EngineController;
use lifecycle::{
    BackgroundLifecycle, LifecycleController, LifecyclePreferences, LifecyclePreferencesStore,
    LifecycleStatus,
};

pub fn run_audio_engine_process() -> Result<(), String> {
    engine_process::run_from_environment()
}

const AUTOSTART_ARGUMENT: &str = "--autostart";
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
    preset: &'static str,
    quit: &'static str,
    stopped_tooltip: &'static str,
    processing_tooltip: &'static str,
    pending_tooltip: &'static str,
}

fn tray_labels(language: UiLanguage) -> TrayLabels {
    match language {
        UiLanguage::ZhCn => TrayLabels {
            show: "打开 XMic",
            start_processing: "启动处理",
            stop_processing: "停止处理",
            cancel_pending: "取消自动启动",
            preset: "处理强度",
            quit: "退出 XMic",
            stopped_tooltip: "XMic - 已停止",
            processing_tooltip: "XMic - 处理中",
            pending_tooltip: "XMic - 等待恢复处理",
        },
        UiLanguage::En => TrayLabels {
            show: "Open XMic",
            start_processing: "Start processing",
            stop_processing: "Stop processing",
            cancel_pending: "Cancel automatic start",
            preset: "Processing strength",
            quit: "Quit XMic",
            stopped_tooltip: "XMic - Stopped",
            processing_tooltip: "XMic - Processing",
            pending_tooltip: "XMic - Waiting to resume",
        },
    }
}

struct TrayMenuState {
    language: AtomicU8,
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

#[tauri::command]
fn get_bootstrap_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    get_engine_status(app, controller)
}

#[tauri::command]
fn list_audio_endpoints() -> Result<Vec<EndpointDescriptor>, String> {
    WasapiPlatform::new()
        .active_endpoints()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_engine_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    let status = engine_status(&controller)?;
    schedule_tray_refresh(&app);
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
    schedule_tray_refresh(&app);
    Ok(())
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

    if let Err(error) = lifecycle.set_start_engine_on_launch(preferences.start_engine_on_launch) {
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
    schedule_tray_refresh(&app);
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
    settings: EngineSettings,
) -> Result<EngineStatus, String> {
    controller.update_settings(settings)?;
    emit_status(&app, &controller)
}

#[tauri::command]
fn start_engine(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
) -> Result<EngineStatus, String> {
    lifecycle.start_manually(&controller)?;
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

fn engine_status(controller: &EngineController) -> Result<EngineStatus, String> {
    list_audio_endpoints().map(|endpoints| controller.status(endpoints))
}

fn emit_status(app: &AppHandle, controller: &EngineController) -> Result<EngineStatus, String> {
    let status = engine_status(controller)?;
    app.emit("engine-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(app);
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
    schedule_tray_refresh(app);
    Ok(status)
}

pub(crate) fn emit_application_state(app: &AppHandle) {
    let controller = app.state::<EngineController>();
    let _ = emit_status(app, &controller);
    let _ = emit_lifecycle_status(app);
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
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn spawn_tray_engine_toggle(app: AppHandle) {
    let _ = thread::Builder::new()
        .name("xmic-tray-action".to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
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

fn schedule_tray_refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || refresh_tray(&handle));
}

fn refresh_tray(app: &AppHandle) {
    let Some(menu) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let controller = app.state::<EngineController>();
    let lifecycle = app.state::<LifecycleController>();
    let running = controller.is_running();
    let settings = controller.settings();
    let pending = lifecycle.has_pending_start();
    let available = lifecycle.is_session_available();
    let labels = tray_labels(UiLanguage::from_code(menu.language.load(Ordering::Relaxed)));
    let (label, tooltip, enabled) = if running {
        (labels.stop_processing, labels.processing_tooltip, true)
    } else if pending {
        (labels.cancel_pending, labels.pending_tooltip, true)
    } else {
        (labels.start_processing, labels.stopped_tooltip, available)
    };

    let _ = menu.show.set_text(labels.show);
    let _ = menu.toggle_processing.set_text(label);
    let _ = menu.toggle_processing.set_enabled(enabled);
    let _ = menu.toggle_aec.set_checked(settings.dsp.aec.enabled);
    let _ = menu.preset_menu.set_text(labels.preset);
    let _ = menu.preset_menu.set_enabled(settings.dsp.aec.enabled);
    let _ = menu
        .preset_natural
        .set_checked(settings.dsp.aec.preset == AecPreset::Natural);
    let _ = menu
        .preset_balanced
        .set_checked(settings.dsp.aec.preset == AecPreset::Balanced);
    let _ = menu
        .preset_strong
        .set_checked(settings.dsp.aec.preset == AecPreset::Strong);
    let _ = menu.toggle_agc2.set_checked(settings.dsp.agc2.enabled);
    let _ = menu
        .toggle_vad_gate
        .set_checked(settings.dsp.vad_gate.enabled);
    let _ = menu.quit.set_text(labels.quit);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

fn launched_from_autostart() -> bool {
    std::env::args_os().any(|argument| argument == OsStr::new(AUTOSTART_ARGUMENT))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
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
            app.manage(LifecycleController::new(LifecyclePreferencesStore::new(
                config_dir.join("app-preferences.json"),
            )));
            setup_tray(app)?;

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

            if !launched_from_autostart() {
                show_main_window(app.handle());
            }
            refresh_tray(app.handle());
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
            set_ui_language,
            list_audio_endpoints,
            update_lifecycle_preferences,
            update_engine_settings,
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
