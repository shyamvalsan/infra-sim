# SOW-0011 - A fleet created after a teardown does not start

## Status

Status: completed

Sub-state: Root cause found in the netdata source and fixed on our side. The
teardown-then-create loop now works with the agent untouched.

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

**Root cause, confirmed in source.** `netdata/netdata @ c23face0bd94`,
`src/plugins.d/plugins_d.c:86-91`:

```c
static void pluginsd_worker_thread_handle_error(struct plugind *cd, int worker_ret_code) {
    if (worker_ret_code == -1) {
        netdata_log_info("... exited abnormally. Disabling it.");
        plugin_set_disabled(cd);
        return;
    }
```

A process killed by a signal reports `-1`, so netdata marks the plugin disabled
and `cd->unsafe.enabled` stays false until the agent restarts. The scan at
`plugins_d.c:364` then skips it.

The theory that the registry ignored a returning file was wrong. The registry
was fine; **our teardown was killing the plugin, and netdata reads that as the
plugin failing.** The defect was ours.

This contradicts `README.md`'s claim that the agent rescans every 60s and no
restart is needed - true for a first install, false after a teardown.

### Acceptance Criteria

1. Root cause confirmed in `netdata/netdata` source, not inferred from
   behaviour. MET.
2. Creating a fleet after a teardown produces a running fleet with no manual
   step. MET.
3. Docs corrected wherever they claim no restart is needed. MET.

## Analysis

Once the cause was known, none of the three options originally sketched was
needed. They all worked around netdata's behaviour; the fix is to stop
triggering it.

A plugin that exits with status 0 takes netdata's *success* path
(`plugins_d.c:61-84`), which sleeps and retries and never disables anything, as
long as it has collected something (`cd->successful_collections`), which ours
always has.

So the plugin exits cleanly on its own when its file disappears, and teardown
gives it a moment to do that before falling back to a signal. The fallback stays
because a wedged collector outliving its own removal has cost this project an
hour of debugging before.

## Pre-Implementation Gate

To be filled before implementation.

## Implications And Decisions

1. **The plugin owns its own shutdown.** It polls its own path each tick and
   exits 0 when the file is gone. One `stat` per second on a path already in
   cache.
2. **Teardown waits before killing.** It removes the file, polls `/proc` for up
   to four seconds, and only signals if the process is still there. The step's
   detail line says which of the two happened, so an operator can tell whether
   the next fleet will start on its own.
3. **No netdata change and no agent restart.** Restarting a prospect's agent
   mid-session was the option to avoid.

## Plan

Delivered as described above.

## Execution Log

### 2026-08-08

- `crates/sim-plugin/src/main.rs`: the collector loop checks its own path each
  tick and returns `Ok(())` when it is gone.
- `crates/sim-console/src/provision.rs`: `wait_for_plugin_exit` and
  `plugin_pids`; teardown removes the file, waits, then falls back to `kill`.
  `stop_plugin` reuses `plugin_pids` instead of walking `/proc` itself.

## Validation

Full loop on live agent `v2.10.0-1030-nightly`, **with the agent never
restarted**:

1. Installed fleet A (`loopa`, 1 node). Went live.
2. Teardown. Step detail read: *"removed; the plugin saw that and exited
   cleanly, so the agent keeps it enabled and will start the next fleet on its
   own"*.
3. Installed fleet B (`loopb`, 1 web + 1 switch) and waited, touching nothing
   else.
4. `loopb-web-01` and `loopb-sw-01` appeared. `netdata` process uptime 5219s
   across the whole sequence, confirming no restart.

Before this change the same sequence left fleet B installed on disk with no
process, no vnodes and nothing logged.

Tests: 188 passed. Clippy and fmt clean.

Same-failure search: `stop_plugin` was the only path sending a signal to the
metrics plugin. The exporter and logs processes are stopped by signal too, but
neither is a netdata-managed plugin, so the disable mechanism does not apply to
them.

Sensitive data gate: none involved.

Artifact maintenance gate: `docs/QUICKSTART.md`'s warning about needing a
restart is removed, since it no longer applies. `README.md` rewritten separately.
Specs unchanged: this is a bug fix, not a behaviour the specs described.

## Outcome

Teardown followed by create works with no manual step. The core SE loop is whole.

## Lessons Extracted

- **A plausible theory about someone else's code is worth less than ten minutes
  in it.** The registry-staleness theory was wrong and would have led to
  restarting the agent from the console, which is the behaviour a demo can least
  afford. The real cause was one `if` statement, and the fix was on our side.
- **How a process dies is part of its contract with its supervisor.** SIGTERM is
  not a clean exit to netdata, and nothing in our code said so until it cost a
  working feature.

## Followup

None. `SOW-0010`'s note about this being an open defect is superseded.

## Regression Log

None yet.
