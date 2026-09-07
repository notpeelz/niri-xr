use std::env;
use std::io;
use std::os::raw::c_int;
use std::process::ExitCode;
use std::sync::Once;
use std::time::Duration;

use eyre::Result;
use openxr as xr;
use tracing::{info, trace, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod cli;
mod uinput;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const WAIT_INTERVAL: Duration = Duration::from_secs(1);

/// Interaction profiles and the controller subpaths bound to the overview
/// action, per hand. Profiles without a binding are skipped.
const OVERVIEW_BINDINGS: &[(&str, Option<&str>, Option<&str>)] = &[
    (
        "/interaction_profiles/oculus/touch_controller",
        Some("/user/hand/left/input/menu/click"),
        None,
    ),
    (
        "/interaction_profiles/valve/index_controller",
        Some("/user/hand/left/input/system/click"),
        Some("/user/hand/right/input/system/click"),
    ),
    (
        "/interaction_profiles/htc/vive_controller",
        Some("/user/hand/left/input/menu/click"),
        Some("/user/hand/right/input/menu/click"),
    ),
    (
        "/interaction_profiles/microsoft/motion_controller",
        None,
        None,
    ),
    ("/interaction_profiles/khr/simple_controller", None, None),
];

fn overview_bound(profile: &str) -> bool {
    OVERVIEW_BINDINGS
        .iter()
        .any(|(p, left, right)| *p == profile && (left.is_some() || right.is_some()))
}

#[cfg(debug_assertions)]
const DEFAULT_LOG_FILTER: &str = "niri_xr=trace";

#[cfg(not(debug_assertions))]
const DEFAULT_LOG_FILTER: &str = "niri_xr=info";

struct OpenXRState {
    instance: xr::Instance,
    session: xr::Session<xr::Headless>,
    left_hand: xr::Path,
    right_hand: xr::Path,
    action_set: xr::ActionSet,
    action_overview: xr::Action<bool>,
}

fn is_retryable(err: xr::sys::Result) -> bool {
    matches!(
        err,
        xr::sys::Result::ERROR_RUNTIME_UNAVAILABLE
            | xr::sys::Result::ERROR_INITIALIZATION_FAILED
            | xr::sys::Result::ERROR_FORM_FACTOR_UNAVAILABLE
    )
}

struct SilenceStderr {
    saved: c_int,
    dev_null: c_int,
}

impl SilenceStderr {
    fn new() -> Self {
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            let dev_null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            libc::dup2(dev_null, libc::STDERR_FILENO);
            SilenceStderr { saved, dev_null }
        }
    }
}

impl Drop for SilenceStderr {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved, libc::STDERR_FILENO);
            libc::close(self.saved);
            libc::close(self.dev_null);
        }
    }
}

fn init_openxr() -> openxr::Result<OpenXRState> {
    let entry = xr::Entry::linked();

    let available_extensions = entry.enumerate_extensions()?;
    let mut extensions = xr::ExtensionSet::default();

    if !available_extensions.mnd_headless {
        return Err(xr::sys::Result::ERROR_EXTENSION_NOT_PRESENT);
    }
    extensions.mnd_headless = true;
    extensions.ext_hp_mixed_reality_controller =
        available_extensions.ext_hp_mixed_reality_controller;

    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "niri-xr",
            application_version: 1,
            ..Default::default()
        },
        &extensions,
        &[],
    )?;
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let (session, _, _) = unsafe {
        instance.create_session::<xr::Headless>(system, &xr::headless::SessionCreateInfo {})
    }?;

    let left_hand = instance.string_to_path("/user/hand/left")?;
    let right_hand = instance.string_to_path("/user/hand/right")?;
    let subaction_paths = [left_hand, right_hand];

    let action_set = instance.create_action_set("main", "Main Bindings", 0)?;
    let action_overview =
        action_set.create_action::<bool>("overview", "Toggle Window Overview", &subaction_paths)?;

    for (profile_path, left, right) in OVERVIEW_BINDINGS {
        let mut bindings = Vec::<xr::Binding>::new();
        if let Some(left) = left {
            bindings.push(xr::Binding::new(
                &action_overview,
                instance.string_to_path(left)?,
            ));
        }
        if let Some(right) = right {
            bindings.push(xr::Binding::new(
                &action_overview,
                instance.string_to_path(right)?,
            ));
        }

        if !bindings.is_empty() {
            instance.suggest_interaction_profile_bindings(
                instance.string_to_path(profile_path)?,
                &bindings,
            )?;
        }
    }
    session.attach_action_sets(&[&action_set])?;

    Ok(OpenXRState {
        instance,
        session,
        left_hand,
        right_hand,
        action_set,
        action_overview,
    })
}

fn main() -> ExitCode {
    let directives = env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_LOG_FILTER.to_owned());
    let env_filter = EnvFilter::builder().parse_lossy(directives);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_writer(io::stderr);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();

    let args = <cli::Args as clap::Parser>::parse();

    match exec(args.wait_xr) {
        Err(err) => {
            eprintln!("Error: {err:?}");
            ExitCode::FAILURE
        }
        Ok(exit_code) => exit_code,
    }
}

fn exec(wait_xr: bool) -> Result<ExitCode> {
    let keyboard = uinput::VirtualKeyboard::new()?;

    let mut attempts = 0_usize;
    let state = loop {
        attempts += 1;
        let result = if attempts > 1 {
            let _silence = SilenceStderr::new();
            init_openxr()
        } else {
            init_openxr()
        };
        match result {
            Ok(state) => break state,
            Err(err) => {
                if !wait_xr || !is_retryable(err) {
                    return Err(err.into());
                }
                static WARN: Once = Once::new();
                WARN.call_once(|| {
                    warn!("XR runtime not available: {err}. Waiting for it to become ready.");
                });
                std::thread::sleep(WAIT_INTERVAL);
            }
        }
    };

    info!("XR runtime ready");

    let mut event_storage = xr::EventDataBuffer::new();
    let mut session_running = false;

    loop {
        while let Some(event) = state.instance.poll_event(&mut event_storage)? {
            #[allow(clippy::single_match)]
            match event {
                xr::Event::SessionStateChanged(e) => match e.state() {
                    xr::SessionState::IDLE => {
                        trace!("session state changed: IDLE");
                    }
                    xr::SessionState::READY => {
                        trace!("session state changed: READY");
                        state
                            .session
                            .begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                    }
                    xr::SessionState::VISIBLE => {
                        trace!("session state changed: VISIBLE");
                        session_running = true;
                    }
                    xr::SessionState::SYNCHRONIZED => {
                        trace!("session state changed: SYNCHRONIZED");
                    }
                    xr::SessionState::FOCUSED => {
                        trace!("session state changed: FOCUSED");
                    }
                    xr::SessionState::STOPPING => {
                        trace!("session state changed: STOPPING");
                        state.session.end()?;
                        session_running = false;
                    }
                    xr::SessionState::LOSS_PENDING => {
                        trace!("session state changed: LOSS_PENDING");
                        return Ok(ExitCode::SUCCESS);
                    }
                    xr::SessionState::EXITING => {
                        trace!("session state changed: EXITING");
                        return Ok(ExitCode::SUCCESS);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if !session_running {
            std::thread::sleep(POLL_INTERVAL);
            continue;
        }

        state
            .session
            .sync_actions(&[xr::ActiveActionSet::new(&state.action_set)])?;

        for hand in [state.left_hand, state.right_hand] {
            let profile = state.session.current_interaction_profile(hand)?;
            if profile == xr::Path::NULL {
                continue;
            }
            let profile = state.instance.path_to_string(profile)?;

            if !overview_bound(&profile) {
                continue;
            }

            let action_state = state.action_overview.state(&state.session, hand)?;
            if action_state.current_state && action_state.changed_since_last_sync {
                trace!("overview triggered");
                keyboard.toggle_overview()?;
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
