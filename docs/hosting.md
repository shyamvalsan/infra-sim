# Hosting the simulator for a team

The console is built to be shared: several people, several simulations, one
host an SRE owns. This is the runbook for hosting it on a Linux box (AWS or
otherwise) reachable from inside your network — and the honest list of what
that exposes.

## The three rules

1. **Set a token.** `INFRA_SIM_TOKEN` on the console process. Without it the
   console refuses to bind anything off-loopback — an open console lets anyone
   who can reach it create, edit and tear down every simulation on the host.
2. **One console per host.** Two consoles race each other's TTL sweeps and
   create slots against the same docker daemon.
3. **Budgets are the contract with the box.** `/etc/infra-sim/console.yaml`
   (fleet size, live simulations, disk, TTL) — refusals quote it, tuning it
   needs no restart. See `docs/operating.md` for the keys.

## Systemd unit

```
# /etc/systemd/system/infra-sim-console.service
[Unit]
Description=Infra-Sim console
After=network-online.target docker.service
Wants=network-online.target

[Service]
ExecStart=/opt/infra-sim/target/release/infra-sim-console --bind 127.0.0.1:19995
EnvironmentFile=/etc/infra-sim/console.env   # INFRA_SIM_TOKEN=...
Restart=on-failure
User=root            # the console drives docker and writes /var/lib/infra-sim

[Install]
WantedBy=multi-user.target
```

The token lives in `/etc/infra-sim/console.env` (`chmod 600`, root-owned) —
never in the unit file, never on a command line, never in the repo.

Loopback bind + a proxy (below) is the recommended shape. A direct
`--bind 0.0.0.0:19995` also works and the token gates it, but you lose TLS
and request logging.

## Reverse proxy with TLS

```
# Caddy (a full TLS config in three lines; nginx equally fine)
sim.internal.example.net {
    reverse_proxy 127.0.0.1:19995
}
```

The console token still applies behind the proxy. If your proxy does
authentication (SSO, mTLS, basic auth), that is defense in depth, not a
replacement — keep both.

## The simulation dashboards

The console is only half the product; the demos happen on each simulation's
own Netdata dashboard, and **a Netdata agent has no authentication of its
own**. By default every simulation binds its dashboard to the host's loopback,
which means remote users cannot open it. Pick one:

**SSH tunnel (default, recommended).** From a laptop:

```bash
ssh -N -L 19988:127.0.0.1:19988 user@sim-host
# then open http://127.0.0.1:19988 as usual
```

Safe, zero configuration, and the port numbers the console prints work
verbatim. For a handful of users this is the whole answer.

**Public binding (opt-in, firewalled hosts only).** In
`/etc/infra-sim/console.yaml`:

```yaml
public_dashboards: true
```

New simulations bind `0.0.0.0:<port>`. This is only acceptable behind a
security group / firewall that admits your team's network, and the create
output warns every time it is active. Anyone who can reach the port can see
that fleet's dashboards and trigger its API — treat the port range
(19900–19990) as internal-only infrastructure.

**Not built (yet): per-simulation auth.** If hosting outgrows tunnels and
firewalls, agent-level auth or a proxy-per-simulation is a real change —
raise it rather than weakening the firewall.

## Updating

```bash
cd /opt/infra-sim && git pull
cargo build --release
sudo ./scripts/sim-docker.sh build    # refresh the simulation image
sudo systemctl restart infra-sim-console
```

Simulations keep running through a console restart; they only restart if
their own image changes.

## What to watch

Three layers, each watching what the others cannot:

**1. The host's own Netdata agent** monitors the box — disk, memory, CPU —
with its stock alarms. Install the simulation-specific extras with one
command:

```bash
sudo ./scripts/host-monitoring.sh install    # alert pack + docker charts
sudo ./scripts/host-monitoring.sh verify     # what is installed, what the agent took
```

What it installs:

| Signal | Where | Meaning / fix |
|---|---|---|
| `infra_sim_unhealthy_containers` | agent alarm | A container keeps failing its health check despite restarts — `docker ps`, then the container's logs (`/tmp/infra-sim-*.log` inside it). |
| Disk space / memory pressure | stock agent alarms | Journals and agent DBs grow until sweeps archive fleets; a warning means tear fleets down or raise the disk budget. |

**2. The console's health endpoint.** `GET /api/health` — unauthenticated by
design, counts and timestamps only — for any uptime monitor:

```json
{"ok": true, "simulations": 4, "max_simulations": 10, "docker": true,
 "disk_used_bytes": 21045239808, "max_disk_bytes": 53687091200,
 "last_sweep_secs_ago": 1420, "sweep_ok": true, "uptime_secs": 86400}
```

Point your monitor at it: `ok:false` means the sweep is stale (>2h) or the
simulation cap is reached. `docker:false` means the daemon is unreachable
from the console.

**3. The console logs.** `journalctl -u infra-sim-console` carries the hourly
sweep heartbeat (`N checked, M archived`) — the fastest place to look when
the endpoint says `sweep_ok: false`.

## Leaving it behind

Tear down fleets in the console (archives to `archive/<name>-<ts>/` on the
host), then `docker system prune` when the box is decommissioned. Claimed
Spaces in Netdata Cloud are deleted from the Cloud UI, not from the host.
