# SOW-0011 - A fleet created after a teardown does not start

## Status

Status: open

Sub-state: Not started. Found during `SOW-0010` live validation and confirmed by
experiment.

## Requirements

### Purpose

"Tear down the last prospect, build the next one" is the core SE loop, and the
second half of it silently does nothing.

### User Request

Raised by the assistant during `SOW-0010`; not yet put to the user.

### Assistant Understanding

Observed on a live agent (`v2.10.0-1030-nightly`):

1. A fleet was created and ran normally.
2. Teardown removed `/etc/netdata/custom-plugins.d/infra-sim.plugin` and stopped
   its process, as designed.
3. A new fleet was created. The plugin file, environment and specs were all
   installed correctly - verified on disk.
4. **The agent never launched the plugin.** No process, no vnodes, no charts.
   `netdatacli dumpconfig` still showed `# infra-sim = yes` and a
   `[plugin:infra-sim]` section with default settings, and
   `enable running new plugins = yes`, `check for new plugins every = 1m`.
5. `sudo systemctl restart netdata` - the plugin started immediately and all
   four nodes appeared.

Working theory, not yet confirmed in source: netdata's plugin registry keeps an
entry for a plugin it has already run, and the periodic scan only launches
plugins it considers *new*. A file that disappears and returns under the same
name is not new.

This contradicts `README.md`'s claim that the agent rescans every 60s and no
restart is needed - true for a first install, false after a teardown.

### Acceptance Criteria

1. Root cause confirmed in `netdata/netdata` source, not inferred from
   behaviour.
2. Creating a fleet after a teardown produces a running fleet with no manual
   step, or the console states plainly what the operator must do and why.
3. Docs corrected wherever they claim no restart is needed.

## Analysis

To be written when the SOW starts. Start at `plugins.d` / `plugins_d_main` and
the registry that backs `[plugin:*]` config sections.

Options to weigh once the cause is known:

- **A.** Teardown leaves the plugin file in place and disarms it another way, so
  the registry entry never goes stale. Avoids the restart entirely; needs a way
  for a plugin with no environment to idle harmlessly rather than exit.
- **B.** The console restarts the agent after an install that follows a
  teardown. Honest and reliable, but restarting a prospect's agent mid-session
  is heavy, and it is exactly the sort of thing that should not happen during a
  demo.
- **C.** The console detects that the plugin has not started within ~90s and
  tells the operator to restart the agent. Smallest change, keeps the failure
  visible rather than silent, but leaves a manual step in the core loop.

## Pre-Implementation Gate

To be filled before implementation.

## Implications And Decisions

Blocked on root cause and a user decision between A, B and C.

## Plan

To be written.

## Execution Log

None yet.

## Validation

Not started.

## Outcome

Not started.

## Lessons Extracted

None yet.

## Followup

None yet.

## Regression Log

None yet.
