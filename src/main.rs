//! Runs the Rust application directly on the machine's native backend.

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() -> impl std::process::Termination {
    env_logger::init();
    platform::run_app(platform::AppSource::Rust(app_root()), app_options())
}

/// `nsh.json`'s `window`, as nsh's generated runner embeds it: the main
/// window only gives the tray an owner and never shows.
fn app_root() -> platform::Root {
    platform::Root::of::<quota_bar::App>()
        .app_id("bar.jyeu.quota")
        .main_window_json(
            r#"{"defaultVisible":false,"defaultSize":{"width":360,"height":520},"titleBar":{"style":"hidden"}}"#,
        )
}

#[cfg(target_os = "macos")]
fn app_options() -> platform::AppOptions {
    platform::AppOptions {
        title: "Quota Bar".to_owned(),
        app_id: "bar.jyeu.quota".to_owned(),
        size: (360, 520),
        activation_policy: platform::ActivationPolicy::Accessory,
        keep_alive_on_close: true,
        ..Default::default()
    }
}

/// Linux has no activation policy or close policy to set: the hidden main
/// window keeps the GTK application alive for the tray.
#[cfg(target_os = "linux")]
fn app_options() -> platform::AppOptions {
    platform::AppOptions {
        title: "Quota Bar".to_owned(),
        app_id: "bar.jyeu.quota".to_owned(),
        size: (360, 520),
        ..Default::default()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "Run this binary on macOS or Linux; use the TSX entry with nsh run for iOS and Android."
);
