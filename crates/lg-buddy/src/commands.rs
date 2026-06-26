use std::env;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Output;
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::events::RuntimeEvent;
use crate::lifecycle::ThreadSleeper;
use crate::lifecycle::{self, JournalctlSleepDetector, NmOnlineNetworkWaiter};
use crate::notifications::{FreedesktopNotifier, Notification, NotificationError, Notifier};
use crate::state::{
    ScreenOwnershipMarker, StateScope, SystemSleepAttemptState, SystemSleepCycleState,
};
use crate::tv::{OledBrightness, TvClient, TvDevice};
use crate::web_os::WebOsTvClient;
use crate::wol::UdpWakeOnLanSender;
use crate::{BrightnessCommand, RunError, StartupMode};

const SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
trait ReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool>;
}

trait BrightnessUi {
    fn prompt_brightness(&self, initial: OledBrightness) -> io::Result<Option<OledBrightness>>;
    fn show_error(&self, title: &str, message: &str) -> io::Result<()>;
}

trait BrightnessCli {
    fn get_brightness(&self) -> Result<OledBrightness, RunError>;
    fn set_brightness(&self, brightness: OledBrightness) -> Result<String, RunError>;
}

struct SystemctlRebootDetector {
    command_path: PathBuf,
}

struct PingReachabilityChecker {
    command_path: PathBuf,
}

struct ZenityBrightnessUi {
    command_path: PathBuf,
}

struct CurrentExeBrightnessCli {
    command_path: PathBuf,
}

struct BrightnessDialogDeps<'a, R, U, B, N> {
    reachability: &'a R,
    ui: &'a U,
    brightness_cli: &'a B,
    notifier: &'a N,
}

impl Default for SystemctlRebootDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for PingReachabilityChecker {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for ZenityBrightnessUi {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemctlRebootDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
        }
    }
}

impl PingReachabilityChecker {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_PING")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ping")),
        }
    }
}

impl ZenityBrightnessUi {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_ZENITY")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("zenity")),
        }
    }
}

impl CurrentExeBrightnessCli {
    fn from_current_exe() -> Result<Self, RunError> {
        Ok(Self {
            command_path: env::current_exe()?,
        })
    }

    fn run(&self, args: &[&str]) -> Result<Output, RunError> {
        ProcessCommand::new(&self.command_path)
            .args(args)
            .output()
            .map_err(RunError::Io)
    }
}

impl BrightnessCli for CurrentExeBrightnessCli {
    fn get_brightness(&self) -> Result<OledBrightness, RunError> {
        let output = self.run(&["brightness", "get"])?;

        if !output.status.success() {
            return Err(RunError::Policy(command_output_message(&output)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        OledBrightness::parse(stdout.trim())
            .map_err(|err| RunError::Policy(format!("invalid output from `brightness get`: {err}")))
    }

    fn set_brightness(&self, brightness: OledBrightness) -> Result<String, RunError> {
        let output = self.run(&["brightness", "set", &brightness.to_string()])?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(RunError::Policy(command_output_message(&output)))
        }
    }
}

impl lifecycle::RebootDetector for SystemctlRebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .arg("list-jobs")
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .any(|line| line.contains("reboot.target") && line.contains("start")))
    }
}

impl ReachabilityChecker for PingReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-c", "1", "-W", "2"])
            .arg(tv_ip.to_string())
            .output()?;

        Ok(output.status.success())
    }
}

impl BrightnessUi for ZenityBrightnessUi {
    fn prompt_brightness(&self, initial: OledBrightness) -> io::Result<Option<OledBrightness>> {
        let output = ProcessCommand::new(&self.command_path)
            .args([
                "--scale",
                "--title=LG TV Brightness",
                "--text=Set OLED Pixel Brightness:",
                "--min-value=0",
                "--max-value=100",
                &format!("--value={initial}"),
                "--step=5",
            ])
            .output()?;

        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let value = OledBrightness::parse(stdout.trim()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid zenity brightness value: {err}"),
            )
        })?;

        Ok(Some(value))
    }

    fn show_error(&self, title: &str, message: &str) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .arg("--error")
            .arg(format!("--title={title}"))
            .arg(format!("--text={message}"))
            .output()?;

        Ok(())
    }
}

fn command_output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return strip_lg_buddy_prefix(&stderr).to_string();
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return strip_lg_buddy_prefix(&stdout).to_string();
    }

    format!(
        "brightness command failed with status {}",
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string())
    )
}

fn strip_lg_buddy_prefix(value: &str) -> &str {
    value.strip_prefix("LG Buddy: ").unwrap_or(value)
}

pub fn run_screen_off<W: Write>(writer: &mut W) -> Result<(), RunError> {
    crate::screen::run_screen_off_from_env(writer)
}

pub fn run_sleep_pre<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_sleep_pre_for_event(
        writer,
        RuntimeEvent::from_command(crate::Command::SleepPre)
            .expect("sleep-pre should map to a runtime event"),
    )
}

pub fn run_sleep_pre_for_event<W: Write>(
    writer: &mut W,
    event: RuntimeEvent,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let cycle_state =
        SystemSleepCycleState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client =
        build_tv_client(&config_path)?.with_command_timeout(SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT);
    let sleeper = ThreadSleeper;

    lifecycle::handle_system_suspend_with(
        writer,
        &config,
        &marker,
        &cycle_state,
        &tv_client,
        &sleeper,
        event,
    )
}

pub fn run_sleep<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(&config_path)?;
    let detector = JournalctlSleepDetector::default();
    let sleeper = ThreadSleeper;

    lifecycle::attempt_legacy_network_manager_sleep_with(
        writer, &config, &marker, &tv_client, &detector, &sleeper,
    )
}

pub fn run_nm_pre_down<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let attempt_state =
        SystemSleepAttemptState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client =
        build_tv_client(&config_path)?.with_command_timeout(SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT);
    let sleeper = ThreadSleeper;
    let mut bus = match crate::session_bus::new_system_bus_client() {
        Ok(bus) => bus,
        Err(err) => {
            return fail_open_nm_pre_down_after_system_bus_error(writer, &attempt_state, err);
        }
    };

    crate::sources::linux::network_manager::handle_pre_down_with(
        writer,
        &config,
        &marker,
        &attempt_state,
        &tv_client,
        &sleeper,
        &mut bus,
    )
}

pub fn run_brightness<W: Write>(
    writer: &mut W,
    command: BrightnessCommand,
) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;

    match command {
        BrightnessCommand::Prompt => {
            let reachability = PingReachabilityChecker::default();
            let ui = ZenityBrightnessUi::default();
            let brightness_cli = CurrentExeBrightnessCli::from_current_exe()?;
            let notifier = FreedesktopNotifier;
            let deps = BrightnessDialogDeps {
                reachability: &reachability,
                ui: &ui,
                brightness_cli: &brightness_cli,
                notifier: &notifier,
            };

            run_brightness_prompt_with(writer, &config, deps)
        }
        BrightnessCommand::Get | BrightnessCommand::Set(_) => {
            let tv_client = build_tv_client(&config_path)?;
            run_brightness_command_with(writer, &config, command, &tv_client)
        }
    }
}

pub fn run_startup<W: Write>(writer: &mut W, mode: StartupMode) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(&config_path)?;
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let network_waiter = NmOnlineNetworkWaiter::default();
    let deps = lifecycle::StartupDeps {
        tv_client: &tv_client,
        wol_sender: &wol_sender,
        sleeper: &sleeper,
        network_waiter: &network_waiter,
    };

    lifecycle::run_startup_with(writer, &config, &marker, deps, mode)
}

pub fn run_system_resume<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let attempt_state =
        SystemSleepAttemptState::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = build_tv_client(&config_path)?;
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let network_waiter = NmOnlineNetworkWaiter::default();

    let result = lifecycle::restore_after_system_sleep_with(
        writer,
        &config,
        &marker,
        &tv_client,
        &wol_sender,
        &sleeper,
        &network_waiter,
    );

    let mut cleanup_error = None;

    if let Err(err) = attempt_state.clear() {
        writeln!(
            writer,
            "LG Buddy System Resume: could not clear system sleep attempt marker after resume. {err}"
        )?;
        cleanup_error = Some(err);
    }

    if let Err(err) = attempt_state.clear_outcome() {
        writeln!(
            writer,
            "LG Buddy System Resume: could not clear system sleep cycle state after resume. {err}"
        )?;
        if cleanup_error.is_none() {
            cleanup_error = Some(err);
        }
    }

    if result.is_ok() {
        if let Some(err) = cleanup_error {
            return Err(RunError::Io(err));
        }
    }

    result
}

fn fail_open_nm_pre_down_after_system_bus_error<W: Write, E: std::fmt::Display>(
    writer: &mut W,
    attempt_state: &SystemSleepAttemptState,
    err: E,
) -> Result<(), RunError> {
    writeln!(
        writer,
        "LG Buddy NetworkManager: could not open system bus for logind PreparingForSleep; failing open. {err}"
    )?;
    if let Err(clear_err) = attempt_state.clear() {
        writeln!(
            writer,
            "LG Buddy NetworkManager: could not clear stale system sleep attempt marker after system bus failure. {clear_err}"
        )?;
    }

    Ok(())
}

pub fn run_shutdown<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let tv_client = build_tv_client(&config_path)?;
    let reboot_detector = SystemctlRebootDetector::default();

    lifecycle::run_shutdown_with(writer, &config, &tv_client, &reboot_detector)
}

pub fn run_screen_on<W: Write>(writer: &mut W) -> Result<(), RunError> {
    crate::screen::run_screen_on_from_env(writer)
}

fn build_tv_client(
    _config_path: &Path,
) -> Result<WebOsTvClient, RunError> {
    Ok(WebOsTvClient::with_defaults())
}

fn run_brightness_command_with<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    command: BrightnessCommand,
    tv_client: &C,
) -> Result<(), RunError> {
    match command {
        BrightnessCommand::Prompt => unreachable!("prompt is handled by the dialog wrapper"),
        BrightnessCommand::Get => {
            let brightness = read_oled_brightness(config, tv_client)?;
            writeln!(writer, "{brightness}")?;
            Ok(())
        }
        BrightnessCommand::Set(brightness) => {
            set_oled_brightness(config, tv_client, brightness)?;
            writeln!(
                writer,
                "LG Buddy Brightness: Set OLED pixel brightness to {brightness}%."
            )?;
            Ok(())
        }
    }
}

fn run_brightness_prompt_with<
    W: Write,
    R: ReachabilityChecker,
    U: BrightnessUi,
    B: BrightnessCli,
    N: Notifier,
>(
    writer: &mut W,
    config: &Config,
    deps: BrightnessDialogDeps<'_, R, U, B, N>,
) -> Result<(), RunError> {
    match deps.reachability.is_reachable(config.tv_ip) {
        Ok(true) => {}
        Ok(false) => {
            let message = format!("TV is not reachable at {}.", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
        Err(err) => {
            let message = format!("Could not check TV reachability at {}. {err}", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
    }

    let initial_brightness = deps
        .brightness_cli
        .get_brightness()
        .unwrap_or(OledBrightness::DEFAULT);

    let Some(brightness) = deps.ui.prompt_brightness(initial_brightness)? else {
        return Ok(());
    };

    match deps.brightness_cli.set_brightness(brightness) {
        Ok(stdout) => {
            write!(writer, "{stdout}")?;
            notify_brightness_success(deps.notifier, brightness)?;
            Ok(())
        }
        Err(err) => Err(notify_brightness_failure(deps.notifier, err)),
    }
}

fn notify_brightness_success<N: Notifier>(
    notifier: &N,
    brightness: OledBrightness,
) -> Result<(), RunError> {
    notifier
        .notify(&Notification::new(
            "LG TV",
            format!("Brightness set to {brightness}%"),
        ))
        .map(|_| ())
        .map_err(|err| {
            RunError::Policy(format!(
                "brightness was set to {brightness}%, but desktop notification failed: {err}"
            ))
        })
}

fn notify_brightness_failure<N: Notifier>(notifier: &N, primary: RunError) -> RunError {
    match notifier.notify(&Notification::new("LG TV", "Failed to set brightness")) {
        Ok(_) => primary,
        Err(notification_err) => append_notification_failure(primary, notification_err),
    }
}

fn append_notification_failure(primary: RunError, notification_err: NotificationError) -> RunError {
    RunError::NotificationAfterPrimary {
        primary: Box::new(primary),
        notification: notification_err,
    }
}

fn read_oled_brightness<C: TvClient>(
    config: &Config,
    tv_client: &C,
) -> Result<OledBrightness, RunError> {
    let tv = TvDevice::new(tv_client, config.tv_ip);
    tv.picture()
        .oled_brightness()
        .map_err(|err| RunError::Policy(format!("failed to read brightness: {err}")))
}

fn set_oled_brightness<C: TvClient>(
    config: &Config,
    tv_client: &C,
    brightness: OledBrightness,
) -> Result<(), RunError> {
    let tv = TvDevice::new(tv_client, config.tv_ip);
    tv.picture()
        .set_oled_brightness(brightness)
        .map(|_| ())
        .map_err(|err| RunError::Policy(format!("failed to set brightness: {err}")))
}

