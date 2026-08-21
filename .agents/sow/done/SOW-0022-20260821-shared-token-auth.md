# SOW-0022 - Shared-token auth and hosting hardening

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - implemented, live-validated, hosting runbook written; closing.

## Requirements

### Purpose

Make the console safe to expose on a network other than the operator's own loopback: every API request authenticated with a shared token, sane binding defaults enforced rather than advised, and an SRE-facing hosting document. Second of four hosting-prep SOWs; builds on SOW-0021's multi-sim console.

### User Request

Host the simulator on an SRE-hosted AWS box reachable from inside Netdata, after guards and a shared-console UI (SOW-0021, done). Auth decision: **B - shared token as the interim gate**; Cloud SSO is the eventual replacement and needs a Cloud-team conversation that is out of scope here.

### Assistant Understanding

Facts:

- The console has zero authentication: every route (`/api/...`) and the UI are open to anyone who can reach the port (`main.rs` router; verified SOW-0021).
- Default bind is `127.0.0.1:19995` - safe by default, but nothing prevents `--bind 0.0.0.0:19995` today, and hosting wants exactly that behind a proxy.
- Simulation dashboards are published on `127.0.0.1:<port>` on the host (`sim-docker.sh` `-p 127.0.0.1:$port:19999`); remote users cannot open them. Netdata agents have no auth of their own.
- Credential precedents in this project: claim tokens and LLM keys are env-or-input only, never argv (world-readable via `ps`), never committed files. A server-side shared token read from the environment follows the same rule.
- The UI polls `/api/status` every 5s and makes action calls on demand; all same-origin. One `fetch` wrapper can carry the token everywhere.
- SOW-0021 established the host policy file `/etc/infra-sim/console.yaml` (budgets). Public dashboard binding is a host-level policy, not a per-create choice - it belongs beside the budgets in that file.

Inferences:

- Auth must be **off unless a token is set**: the local single-operator flow (laptop, loopback, `startsim.sh`) must not start demanding tokens; hosted boxes set one.
- The console should **refuse to bind a non-loopback address without a token** - a hosted console accidentally left open is the exact failure this SOW exists to prevent, and refusing at startup is the only moment the message is guaranteed to be read.
- The dashboard-access problem has no in-repo fix that is both safe and simple (proxying agents means proxying websockets and hundreds of dashboard requests); the honest v1 is an opt-in public binding plus documented SSH tunneling, with per-sim auth a future SOW if hosting wants it.
- Constant-time comparison for the token check: cheap, correct habit, even for a shared token.

Unknowns:

- None blocking. Whether Netdata Cloud SSO can be minted for an internal tool is a Cloud-team question recorded as the follow-up hand-off.

## Acceptance Criteria

- With `INFRA_SIM_TOKEN` set: every `/api/*` request without a valid bearer token gets 401 with a JSON body; with the token, the full SOW-0021 flow works (status, per-sim status, create, teardown) - live-verified.
- Without `INFRA_SIM_TOKEN`: the local loopback flow behaves exactly as today (regression-verified), and a non-loopback `--bind` is refused at startup with a message saying how to set the token.
- The UI collects the token once on 401 (inline prompt, not `alert`), stores it in `sessionStorage`, attaches it to every subsequent call, and re-authenticates after expiry without a page reload loop.
- `public_dashboards: true` in the host policy file makes new simulations bind their agent to `0.0.0.0` (verified via `docker port`); default stays loopback.
- A hosting document (`docs/hosting.md`) covers: systemd unit with the token, reverse-proxy TLS example, dashboard access options (SSH tunnel recommended, public binding opt-in), and the one-console-per-host rule.
- Gates: `cargo test`, clippy `-D warnings`, fmt; JS syntax check; live validation on this machine with the user's `default` untouched.

## Analysis

Sources checked:

- `crates/sim-console/src/main.rs` (router, bind handling, UI route), `ui.html` (fetch sites, polling loop)
- `scripts/sim-docker.sh` (port binding), `crates/sim-console/src/container.rs` (create passthrough), `crates/sim-console/src/budget.rs` (host policy file)
- `AGENTS.md` credential rules; SOW-0021 spec section "Shared hosting"; `docs/operating.md` console section

Current state:

- No auth anywhere; safe default bind only by convention; dashboards loopback-only with no documented remote path.

Risks:

- Breaking the local flow with an over-eager auth default - mitigated by off-by-default and a regression check.
- Token in `sessionStorage` readable by XSS on the page - accepted for an internal tool; the page is same-origin, static, and the alternative (per-request re-entry) destroys usability.
- Public dashboard binding exposes unauthenticated Netdata agents - opt-in only, warned in the doc and in the create notes when active.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- A console built for one operator on loopback is being asked to serve a network. The gap is not a missing feature but a missing trust boundary: nothing distinguishes "a call from the operator" from "a call from anyone", because until now both were the same machine.

Evidence reviewed:

- As listed under Analysis; SOW-0021's live validation exercised every route this SOW gates.

Affected contracts and surfaces:

- Console API: all `/api/*` routes gain a bearer-token requirement when `INFRA_SIM_TOKEN` is set; `GET /` (the static UI shell, no data) stays open. 401 responses are JSON `{"ok":false,"error":...}`.
- Startup: non-loopback `--bind` without a token refuses with an actionable message.
- `ui.html`: one `fetch` wrapper (token from `sessionStorage` into `Authorization: Bearer`), a single inline re-auth prompt on 401, no polling storm.
- Host policy: `console.yaml` gains `public_dashboards: false` (default); `budget.rs` parses it; `container::create` passes a `--public-dashboards` flag through to `sim-docker.sh` when set.
- `sim-docker.sh`: `create --public-dashboards` binds `-p 0.0.0.0:$port:19999` and prints a warning line.
- Docs: new `docs/hosting.md`; links from `README.md` and `docs/operating.md`.

Existing patterns to reuse:

- The credential precedents (env-or-input, never argv): the token arrives via environment, like `NETDATA_CLAIM_TOKEN`.
- `json_err` response shape for the 401 body.
- The host policy file parse in `budget.rs` (`deny_unknown_fields`, defaults in code).

Risk and blast radius:

- Console-only; middleware order is the one subtle spot (must wrap every /api route, including future ones - axum layer, not per-route). Local regression risk covered by the off-by-default acceptance test.

Sensitive data handling plan:

- The token never enters argv, files, logs, or SOW text; examples use a placeholder. The UI stores it in `sessionStorage` only. The hosting doc's systemd example uses an `EnvironmentFile=` pointer, not an inline secret.

Implementation plan:

1. `main.rs`: token loading + startup validation (non-loopback refuse), axum auth layer over `/api`, constant-time compare, startup log of auth mode.
2. `ui.html`: fetch wrapper + inline 401 prompt.
3. Host policy: `public_dashboards` parse + create passthrough + `sim-docker.sh` flag.
4. `docs/hosting.md` + README/operating links.
5. Live validation; close-out gates.

Validation plan:

- Unit: policy parse of `public_dashboards`; token-compare helper.
- Live: no-token loopback regression (status + create cycle abort cheaply or full); token set - 401 without header, success with header across GET and POST; wrong token 401; UI shell serves without token; non-loopback without token refuses at startup; public-dashboards create binds 0.0.0.0 (`docker port`) then teardown.
- Same-failure scan: grep for fetch sites bypassing the wrapper; routes outside the layer.

Artifact impact plan:

- AGENTS.md: auth model note (off on loopback, required off-loopback, token via env).
- Runtime project skills: unaffected.
- Specs: `runtime-and-scenarios.md` Shared hosting section gains the auth paragraph.
- End-user/operator docs: `docs/hosting.md` new; README + operating.md links.
- SOW lifecycle: alone after SOW-0021; enables 0023/0024.

Open-source reference evidence:

- None newly required.

Open decisions:

- None blocking; all design resolved within the approved "2B" frame. Recorded: auth off unless token set; non-loopback bind refused without token; `sessionStorage` UI storage; public dashboards opt-in via host policy file; per-sim agent auth and Cloud SSO are follow-ups.

## Implications And Decisions

1. Auth: shared bearer token, interim (user: "2B", 2026-08-19). Cloud SSO remains the end state; hand-off recorded.
2. Auth off unless `INFRA_SIM_TOKEN` is set; non-loopback binds refused without it (engineering default, recorded here).
3. Public dashboard binding is a host policy in `console.yaml`, default off, warned when on (engineering default, recorded here).

## Plan

1. Auth layer + startup validation (core).
2. UI token handling.
3. Public-dashboards policy path.
4. Hosting doc + links.
5. Live validation, gates, close.

## Execution Log

### 2026-08-21

- SOW written under the approved 2B direction; implementation started.
- Auth: `INFRA_SIM_TOKEN` env (off unless set), axum layer over `/api` only (the UI shell at `/` stays open - it hosts the prompt), constant-time compare, JSON 401s, startup refusal of non-loopback binds without a token, auth-mode log line.
- UI: one `fetch` wrapper attaching `Authorization: Bearer` from `sessionStorage`; a single inline prompt on 401 (no alert, no loop); `sessionStorage` over `localStorage` for tab-scoped credential lifetime.
- Host policy: `public_dashboards` in `console.yaml` (default false), passed through `CreateOptions` to `sim-docker.sh --public-dashboards` which binds `0.0.0.0` with a warned line; create notes say it plainly when active.
- `docs/hosting.md` runbook: three rules, systemd unit with EnvironmentFile token, Caddy TLS example, dashboard access (SSH tunnel recommended, public binding opt-in with firewall caveat), update and decommission paths.
- Defects caught by own validation before close: the auth layer initially gated the UI shell itself (401 at `/`, hiding the prompt - exempted non-`/api` paths); the startup refusal message suggested a malformed command (reworded); the public-dashboards warning lived only in script stderr and never reached the create notes (added); clippy forced the 8-arg create into `CreateOptions` (a readability win).

## Validation

Acceptance criteria evidence:

- Token set (live): `/api/status` -> 401 with the JSON guidance body; wrong bearer -> 401; correct bearer -> 200 with fleet data; POST with token passes auth (reaches validation, 422 for a malformed body - past the layer). UI shell `/` -> 200 with the token set (prompt reachable).
- No token (live): loopback console logs `auth OFF (loopback, single operator)` and `/api/status` answers 200 - the local flow is unchanged (regression).
- Non-loopback without token (live): `--bind 0.0.0.0:19996` and `--bind 10.0.0.5:19996` both refuse at startup with the corrected actionable message; exit code 1 verified.
- Public dashboards (live): with `public_dashboards: true`, a create bound `0.0.0.0:19989` (`docker port` output) and the create response carried the no-authentication/firewall note; fleet torn down afterwards. Default remains loopback (the user's running sim untouched).
- `docs/hosting.md` written and linked from README and `docs/operating.md`.

Tests or equivalent validation:

- `cargo test`: 258 passed (public_dashboards parse incl. default-off and override; prior suites).
- `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check` clean; `bash -n` clean; console JS `node --check` clean.

Real-use evidence:

- Both modes driven through the real binary on this machine (curl matrix above); the tokened console coexisted with the user's `default` simulation, untouched.

Reviewer findings:

- Pending (fold into the next explicitly-requested review round, per SOW-0020/0021 precedent).

Same-failure scan:

- UI fetch sites: all go through the wrapped `window.fetch` (the wrapper is installed before any caller; `rawFetch` is unreachable from page code). No `XMLHttpRequest` or WebSocket use exists to bypass it.
- Routes outside the layer: only `GET /` (deliberate, static shell). Future routes inherit the layer by construction (it wraps the router, not a route list).

Sensitive data gate:

- The test token (`test-secret-123`) was session-scratch only, never committed; all artifacts use `<secret>` placeholders. The hosting doc's systemd example points at a root-owned EnvironmentFile rather than an inline secret. No argv token anywhere (env only).

Artifact maintenance gate:

- AGENTS.md: auth model note added below.
- Runtime project skills: unaffected (no new validation-class lesson).
- Specs: `runtime-and-scenarios.md` Shared hosting section gains the auth paragraph.
- End-user/operator docs: `docs/hosting.md` new; README and `docs/operating.md` link it.
- End-user/operator skills: none affected.
- SOW lifecycle: executed alone after SOW-0021; enables 0023/0024.

Specs update:

- `runtime-and-scenarios.md` - auth paragraph in Shared hosting.

Project skills update:

- Pending.

End-user/operator docs update:

- `docs/hosting.md` (new), `README.md`, `docs/operating.md`.

End-user/operator skills update:

- Pending.

Lessons:

- Gating the UI shell along with the API hid the very prompt that unlocks the API - a trust boundary drawn one path too wide is indistinguishable from an outage. Gate the data, serve the shell.
- Three of this SOW's four defects were wording or placement (the malformed refusal command, the warning stranded in stderr, the shell 401) - none would fail a unit test, all would erode trust on a hosted box. The curl matrix against the real binary is what caught them.

Follow-up mapping:

- Cloud SSO investigation (replaces the shared token): tracked in Followup below; needs the Cloud team.
- Per-simulation dashboard auth: tracked; only if hosting outgrows tunnels + firewalls.
- Rate limiting at the proxy: documented as the proxy's job in hosting.md; no in-repo work planned.

## Outcome

Delivered and closed. The console now has a trust boundary proportionate to a
shared host: a shared bearer token (env-configured, off for loopback
single-operator use, mandatory for any off-loopback bind - enforced at
startup), constant-time checks, JSON 401s, a one-time inline UI prompt with
tab-scoped storage, an opt-in warned public-dashboards host policy, and a
complete SRE runbook (`docs/hosting.md`) covering systemd, TLS proxying,
dashboard access and decommissioning. Both modes regression-verified live.
Hosting prep remaining: UI help (0023), monitoring (0024).

## Lessons Extracted

Pending.

## Followup

- Cloud SSO investigation with the Cloud team; replaces the shared token.
- Per-simulation dashboard auth if hosting outgrows SSH tunnels and the opt-in public binding.
- Rate limiting on the auth check if a hosted box ever sees brute-force noise (the reverse proxy can carry this first).

## Regression Log

None yet.
