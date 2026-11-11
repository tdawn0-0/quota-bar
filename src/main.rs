//! Runs the Rust application directly on the machine's native backend.

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() -> impl std::process::Termination {
    env_logger::init();
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<quota_bar::App>()),
        platform::AppOptions {
            title: "Quota Bar".to_owned(),
            app_id: "bar.jyeu.quota".to_owned(),
            size: (360, 520),
            activation_policy: platform::ActivationPolicy::Accessory,
            keep_alive_on_close: true,
            ..Default::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "Run this binary on macOS or Linux; use the TSX entry with nsh run for iOS and Android."
);
