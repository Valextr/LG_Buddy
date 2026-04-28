# LG Buddy Runtime Event Handler Map

This document maps the top-level events LG Buddy consumes and the handler paths
that act on them.

It complements [session-backend-model.md](session-backend-model.md). The session
backend model defines canonical session semantics. This document describes how
real system, desktop, and user-service entrypoints reach runtime policy.
Product-wide defaults and advanced configuration rules are documented in
[defaults-and-configuration.md](defaults-and-configuration.md).

## Event Vocabulary

LG Buddy has three related but distinct event shapes.

| Shape | Owner | Purpose |
| --- | --- | --- |
| Command entrypoint | `lib.rs` / `commands.rs` / `session::runner` | External service, hook, or user command invokes one runtime command. |
| Session event | `session.rs` | Backend-neutral event such as `Idle`, `Active`, `WakeRequested`, `BeforeSleep`, or `AfterResume`. |
| Inactivity observation | `session/inactivity.rs` | Lower-level inactivity fact such as idletime, provider idle, wake request, or user activity. |

The command entrypoint layer remains the external integration surface. The
session event layer is active for native monitor behavior and for system
lifecycle handling. The inactivity observation layer owns edge-triggered blank
and restore decisions for native idle/activity integrations.

GNOME is the production pilot for the native inactivity path today, but the
model is not GNOME-specific. A future non-GNOME Wayland adapter should feed the
same normalized inactivity observations.

## Current Top-Level Handlers

| External event source | Runtime entrypoint | Primary handler | Current action |
| --- | --- | --- | --- |
| system boot / service start | `lg-buddy startup boot` | `commands::run_startup` | Send Wake-on-LAN and restore the configured input. |
| system shutdown / service stop | `lg-buddy shutdown` | `commands::run_shutdown` | Power off the TV when the configured input is active, unless a reboot is pending. |
| logind `PrepareForSleep(true)` | `lg-buddy lifecycle` | `session::runner` -> `BeforeSleep` | Run pre-sleep TV power-off policy, then release the logind delay inhibitor. |
| logind `PrepareForSleep(false)` | `lg-buddy lifecycle` | `session::runner` -> `AfterResume` | Run wake restore policy, then reacquire the logind delay inhibitor. |
| user graphical session start | `lg-buddy monitor` | `session::runner::run_monitor` | Detect the session backend and run the selected monitor path. |
| manual screen blank | `lg-buddy screen-off` | `commands::run_screen_off` | Blank or power off the TV if LG Buddy owns the configured input. |
| manual screen restore | `lg-buddy screen-on` | `commands::run_screen_on` | Restore the screen when marker and restore-policy rules allow it. |

Compatibility command surfaces still exist for direct/manual invocation:

| Command | Current role |
| --- | --- |
| `lg-buddy sleep-pre` | Pre-sleep policy command used by the lifecycle runner. |
| `lg-buddy startup wake` | Wake restore policy command used by the lifecycle runner. |
| `lg-buddy sleep` | Legacy NetworkManager pre-down behavior. It is not installed as a default event handler. |

These handlers are intentionally conservative around ownership:

- session-scope screen blanking uses the session marker
- system sleep uses the system marker
- restore behavior is gated by `screen_restore_policy`
- shutdown does not write ownership markers

## Runtime Event Pipeline

LG Buddy is moving toward one source-agnostic event pipeline:

```text
system lifecycle sources
desktop idle/activity sources
auxiliary activity sources
  -> narrow source adapters
  -> normalized session events / inactivity observations
  -> InactivityEngine
  -> lifecycle and panel-protection policy
  -> TV action executor
```

Source adapters report facts. They do not own marker semantics, restore policy,
retries, Wake-on-LAN, or TV transport behavior.

Examples:

| Source category | Example source | Runtime representation |
| --- | --- | --- |
| system lifecycle | `org.freedesktop.login1`, platform-native lifecycle APIs | `BeforeSleep`, `AfterResume` |
| desktop idle/activity | Mutter, native Wayland idle protocols | idletime and activity observations |
| desktop wake request | GNOME ScreenSaver wake signal, future equivalents | `WakeRequested` |
| auxiliary activity | Linux gamepad input | `UserActivityObserved` |

## Monitor Event Paths

### Native Inactivity Path

The native inactivity path is the intended path for desktop adapters that can
report activity facts directly. GNOME is the first production backend on this
path. A future non-GNOME Wayland adapter should feed the same inactivity model
instead of delegating blank/restore commands to an external tool.

```text
native desktop activity facts
auxiliary activity facts
  -> inactivity observations
  -> InactivityEngine
  -> Idle / Active / WakeRequested / UserActivity
  -> screen-off / screen-on command policy
```

Current pilot inputs:

| Provider surface | Runtime representation | Consumed by |
| --- | --- | --- |
| `org.gnome.ScreenSaver.ActiveChanged(true)` | `ProviderIdle` | `InactivityEngine` |
| `org.gnome.ScreenSaver.ActiveChanged(false)` | `ProviderActive` | `InactivityEngine` |
| `org.gnome.ScreenSaver.WakeUpScreen` | `WakeRequested` | `InactivityEngine` |
| `org.gnome.Mutter.IdleMonitor.GetIdletime` | `IdleTimeMs` | `InactivityEngine` |
| Linux gamepad activity | `UserActivityObserved` | `InactivityEngine` |

These are GNOME-specific source surfaces, but the runtime representations are
backend-neutral. The key architectural point is that blank/restore decisions are
made after normalization, not inside the GNOME adapter.

The resulting decisions are dispatched as:

| Inactivity decision | Dispatched event | Command policy |
| --- | --- | --- |
| `BlankNow` | `SessionEvent::Idle` | `screen-off` |
| `RestoreNow` from provider active | `SessionEvent::Active` | `screen-on` |
| `RestoreNow` from wake request | `SessionEvent::WakeRequested` | `screen-on` |
| `RestoreNow` from idletime or auxiliary activity | `SessionEvent::UserActivity` | `screen-on` |

### Delegated `swayidle` Path

The `swayidle` monitor is still a legacy delegated path.

```text
swayidle timeout/resume
  -> external command string
  -> lg-buddy screen-off / lg-buddy screen-on
```

`swayidle.rs` models hook-to-`SessionEvent` mapping, including
`BeforeSleep`, `AfterResume`, `Lock`, and `Unlock`, but the production monitor
currently starts `swayidle` with direct `screen-off` and `screen-on` commands.
Those richer hook events are not consumed by the monitor runner.

This path exists for current non-GNOME Wayland support. It should not define the
long-term architecture. Retiring it means replacing delegated timeout/resume
execution with native idle/activity facts that feed the same inactivity engine
used by the current native path.

## System Lifecycle Event Handling

LG Buddy handles system lifecycle through one Linux lifecycle owner:
`LG_Buddy_lifecycle.service` running `lg-buddy lifecycle`.

```text
org.freedesktop.login1 PrepareForSleep
  -> logind adapter
  -> SessionEvent::BeforeSleep / SessionEvent::AfterResume
  -> lifecycle policy
  -> TV action executor
```

The lifecycle service subscribes to logind manager signals on the system bus and
holds a logind sleep delay inhibitor while it is ready to handle sleep. On
`PrepareForSleep(true)`, it runs the pre-sleep policy and releases the inhibitor
so suspend can continue. On `PrepareForSleep(false)`, it runs wake restore
policy and reacquires the inhibitor.

The installer must not leave a second lifecycle owner active. It removes or
disables these legacy artifacts during install and uninstall:

- `LG_Buddy_sleep.service`
- `LG_Buddy_wake.service`
- old unit override directories for those services
- `/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep`
- `/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook`

Current lifecycle signal mapping:

| logind surface | Canonical event | Runtime action |
| --- | --- | --- |
| `PrepareForSleep(true)` | `BeforeSleep` | `run_sleep_pre` |
| `PrepareForSleep(false)` | `AfterResume` | `run_startup(..., StartupMode::Wake)` |

The current `SessionEventDispatcher` handles:

| Session event | Current action |
| --- | --- |
| `Idle` | Run `screen-off`. |
| `Active` | Run `screen-on`. |
| `WakeRequested` | Run `screen-on`. |
| `UserActivity` | Run `screen-on`. |
| `BeforeSleep` | Run pre-sleep TV power-off policy. |
| `AfterResume` | Run wake restore policy. |
| `Lock` | Log as unhandled. |
| `Unlock` | Log as unhandled. |

## Lifecycle Default And Migration Stance

The general default/configuration stance is defined in
[defaults-and-configuration.md](defaults-and-configuration.md). Applied to the
lifecycle path:

- automatic system sleep/wake TV control defaults to enabled
- users who do not want automatic sleep/wake TV control opt out through
  `system_sleep_wake_policy=disabled`
- default installs do not ask whether lifecycle automation should run
- legacy systemd and NetworkManager sleep/wake handlers are cleanup targets, not
  parallel runtime handlers
- legacy cleanup honors a persisted opt-out config value

## Target Non-GNOME Wayland Shape

Native non-GNOME Wayland idle work is separate from logind lifecycle work and
should follow the same native inactivity path.

Target event path:

```text
Wayland idle/activity facts
  -> native Wayland adapter
  -> inactivity observations
  -> InactivityEngine
  -> screen-off / screen-on command policy
```

That keeps the responsibilities separate:

- logind reports machine lifecycle
- desktop adapters report activity facts
- native Wayland reports Wayland idle/activity facts
- gamepad input reports auxiliary user activity
- LG Buddy policy decides when those facts should blank or restore the TV

## Remaining Migration Notes

The current logind slice establishes the lifecycle source and removes default
installation of the legacy systemd/NetworkManager lifecycle handlers. Remaining
work should stay scoped:

1. Keep native Wayland idle replacement separate from the logind lifecycle path.
2. Retire the delegated `swayidle` monitor once native non-GNOME Wayland
   activity facts are available.
3. Preserve the one-lifecycle-owner invariant in installer, release-bundle, and
   uninstall tests.
4. Treat future platform lifecycle providers, such as a possible macOS provider,
   as source adapters that emit the same canonical lifecycle events.
