use std::io::Write;

use crate::config::Config;
use crate::events::RuntimeEvent;
use crate::lifecycle::{self, Sleeper};
use crate::session_bus::SessionBusClient;
use crate::state::{ScreenOwnershipMarker, SystemSleepAttemptState};
use crate::tv::TvClient;
use crate::RunError;

use super::logind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkTeardownEvent {
    pub(crate) event: RuntimeEvent,
    pub(crate) phase_read_error: Option<String>,
}

impl NetworkTeardownEvent {
    fn new(machine_sleep_pending: Option<bool>, phase_read_error: Option<String>) -> Self {
        Self {
            event: RuntimeEvent::network_teardown_imminent(machine_sleep_pending),
            phase_read_error,
        }
    }
}

pub(crate) fn network_teardown_event_from_logind_property<B: SessionBusClient>(
    bus: &mut B,
) -> NetworkTeardownEvent {
    match logind::preparing_for_sleep(bus) {
        Ok(preparing) => NetworkTeardownEvent::new(Some(preparing), None),
        Err(err) => NetworkTeardownEvent::new(None, Some(err.to_string())),
    }
}

pub(crate) fn handle_pre_down_with<W: Write, C: TvClient, Sl: Sleeper, B: SessionBusClient>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
    bus: &mut B,
) -> Result<(), RunError> {
    let event = if config.system_sleep_wake_policy.is_enabled() {
        network_teardown_event_from_logind_property(bus)
    } else {
        NetworkTeardownEvent::new(None, None)
    };

    lifecycle::handle_network_teardown_with(
        writer,
        config,
        lifecycle::NetworkTeardownDeps {
            marker,
            attempt_state,
            tv_client,
            sleeper,
        },
        event.event,
        event.phase_read_error.as_deref(),
    )
}
