# SOW-0009 - Netdata's own inference gateway as an LLM provider

## Status

Status: completed

Sub-state: Delivered. `--llm netdata` reads `llm.netdata.cloud`, the key comes
from a gitignored `.env`, and the model roster was measured rather than assumed.

## Requirements

### Purpose

Free-form description reading depended on an Anthropic or OpenAI key that no SE
has to hand. Netdata runs its own inference gateway, which removes the external
dependency and the per-seat key problem.

### User Request

"instead of openai/anthropic lets use netdata's own locally hosted models.. i
have put the api key in .env and you can use llm.netdata.cloud for inference,
can you test out deepseek v4 flash model and see if its good for this use-case"
(2026-08-08)

### Assistant Understanding

Three pieces: a provider pointing at the gateway, key resolution from `.env`,
and an evidence-based answer on whether `deepseek-v4-flash` is fit for this.

### Acceptance Criteria

1. `--llm netdata` and the console's provider picker both reach
   `llm.netdata.cloud`. MET.
2. The key is read from `.env` without being logged, exported, or committed.
   MET.
3. A measured verdict on `deepseek-v4-flash`, against the real request rather
   than a toy one. MET - it is not fit, and the evidence is below.
4. A default model that works, with the reason recorded next to it. MET - `k3`.

## Analysis

The gateway is OpenAI-compatible (`/v1/models`, `/v1/chat/completions`, bearer
auth), so the provider shares every request, response and auth branch with
`Provider::OpenAi`. Only the host, key variable and default model differ.

The interesting question was not wiring but capability. The whole
description-to-plan contract rests on the provider honouring a **strict
`json_schema` response format**: the model returns a plan constrained to roles
and services that exist, and code renders it. A model that answers in prose
does not degrade gracefully - it fails outright.

## Pre-Implementation Gate

**Problem / root-cause model.** `Provider` had exactly two variants, both
external vendors. Key resolution read only the process environment, which
`sudo` strips - so the console, which runs as root, could never see a key the
operator had exported.

**Evidence reviewed.**

- `GET https://llm.netdata.cloud/v1/models` - three models:
  `deepseek-v4-flash`, `glm-5.2-max`, `k3`.
- A minimal `json_schema` probe returned `{"cache":"redis"}` cleanly, which is
  what made the full-request failure worth chasing rather than assuming.
- `crates/sim-engine/src/llm.rs` - `Provider`, `Config::url`, `curl_config`,
  `openai_text`, `plan_schema`.
- `crates/sim-engine/src/describe.rs` - `available_services` reads one
  directory, which is how the LLM path came to be offered 5 services while the
  keyword path had 261.

**Affected contracts and surfaces.** `Provider` enum and its parse/label/
endpoint tables, `Config` (gains `repo`), key resolution, console
`/api/catalogue` (`llm_providers`), console UI dropdown, `--llm` help text,
README, quickstart, `authoring-environments.md`.

**Existing patterns to reuse.** The OpenAI wire format; the curl-with-secret-on-
stdin call path; the JSON-schema-enum-plus-revalidation plan contract.

**Risk and blast radius.** The key is a credential. It must not reach argv, a
log, a committed file, or the DOM. `.env` is already gitignored. Setting
process environment variables was considered and rejected: `set_var` is unsafe
and this crate forbids unsafe - the compiler refused it, correctly.

**Sensitive data handling plan.** No key value appears in this SOW, in any
commit, in any error message, or in debug output. The `.env` reader never logs
values. `INFRA_SIM_LLM_DEBUG=1` prints the request body and response, neither of
which carries the key - it travels in a curl config file on stdin.

**Implementation plan.** Provider variant → `.env` key resolution → give the LLM
path the full catalogue → measure the model roster → default and error message
→ docs.

**Validation plan.** Drive the real `--describe` path and the console's
`/api/describe` against the gateway. Compare all three models on the actual
request. Lint a fleet the model produced.

**Artifact impact plan.** `README.md`, `docs/QUICKSTART.md`,
`.agents/sow/specs/authoring-environments.md`.

**Open decisions.** None. The user named the provider and the model to test;
the model verdict is evidence, not a design choice.

## Implications And Decisions

1. **`k3` is the default for this provider, not `deepseek-v4-flash`.** Measured,
   with the table in the spec and the reason in a comment beside the constant.
2. **The key may come from `.env`.** The console runs under `sudo`, which strips
   the environment; a web form would put the key in the DOM and in browser
   history. `.env` is gitignored and read without logging.
3. **Nothing is written back into the process environment.** `set_var` is
   unsafe, the crate forbids unsafe, and `environ` is readable by anything that
   can see the process.
4. **The provider picker only offers what it can reach.** A provider whose key
   is missing would fail on the first request; offering it is a trap.

## Plan

Delivered as described in the gate.

## Execution Log

### 2026-08-08

- `Provider::Netdata` sharing the OpenAI wire format; `LLM_API_KEY`,
  `NETDATA_LLM_BASE_URL`, default model `k3`.
- `llm::env_file` and `llm::available`; `Config::repo`; key resolution falls
  back to `.env`.
- `llm::installable_services` so the model's vocabulary is the full catalogue.
- `served_model` and a JSON-parse error that names the model that replied.
- `INFRA_SIM_LLM_DEBUG=1` to dump request and response.

### Findings during implementation

- **`deepseek-v4-flash` is not served.** The gateway answers with `MiniMax-M3` -
  3 runs out of 3 - and that model ignores the strict `json_schema` response
  format, replying with prose containing a fenced code block. The describe
  command failed with "the model's reply was not valid JSON" and no indication
  that a different model had answered.
- **`glm-5.2-max` ignores it too**, under its own name.
- **A toy request is not evidence.** A 5-value enum with a one-line prompt
  returned clean schema-constrained JSON from the same endpoint. Only the real
  request - 262-service enum, ~7.7k-character system prompt - exposed the
  failure. Testing the wiring rather than the payload would have shipped this.
- **The LLM path was offered 5 services where the keyword path had 261.**
  `available_services` reads a single directory, and `specs/generated/` was
  never added to the model's vocabulary in `SOW-0006`. The symptom was a model
  correctly reporting HAProxy as unmodellable - a *right* answer to the *wrong*
  question, which is why it read as model weakness rather than a plumbing bug.
- **The compiler refused the first design.** Loading `.env` into the process
  environment via `set_var` is unsafe; the crate's `forbid(unsafe_code)` caught
  it and forced key resolution to be explicit instead.
- **Cloudflare blocks Python's default user agent** on this gateway, so the
  first test harness got 403 while `curl` succeeded - incidental to the harness,
  but a useful confirmation that the production path shells out to curl.

## Validation

**Model comparison**, identical real request (262-service enum, ~7.7k-char
system prompt), same description:

| Model asked for | Actually answered | Structured output | Time | Output tokens |
|---|---|---|---|---|
| `k3` | `k3` | honoured | 10s | 403 |
| `glm-5.2-max` | `glm-5.2-max` | ignored | 24s | 1850 |
| `deepseek-v4-flash` | `MiniMax-M3` | ignored | 24s | 1396 |

`deepseek-v4-flash` re-run 3x: `MiniMax-M3` every time, schema ignored every
time.

**Plan quality**, `k3` on five realistic prospect descriptions - all five
returned schema-valid plans in 14-24s:

- 40-instance AWS estate: 2 lb/**haproxy** (not the nginx default), 20
  web/**tomcat** inferred from "Java app servers", 3 db/postgres, 2 cache/redis;
  RabbitMQ and Elasticsearch reported unsupported.
- EKS estate: 3 k8s-control-plane, 12 k8s-worker, 2 db/**mysql** for the legacy
  VMs, 1 lb/nginx ingress.
- "One web server, one database": exactly that.
- Telco: 12 web/**freeradius+unbound**, with "200 switches under SNMP polling"
  correctly reported as having no matching role.
- Windows shop: 2 web/active-directory, 2 web/ms-exchange, 3
  db/microsoft-sql-server, 8 web/**iis+asp-net**.

**End to end**: `--describe ... --llm netdata` produced a 26-node fleet in 15s
which then **passed the 6-hour fidelity lint** with no violations and no pinned
signals. The console's `/api/describe` returned the Windows-shop plan in 23s
with `source: k3`, and `/api/catalogue` advertised `["netdata"]` - resolved from
`.env`, with no key exported into the environment.

**Error path**: forcing `--llm-model deepseek-v4-flash` now reports "Asked for
'deepseek-v4-flash' but 'MiniMax-M3' answered - this gateway aliases or falls
back, and the substitute does not honour structured output."

**Tests**: 180 passed, 0 failed - three new, covering the gateway endpoint, that
reading a `.env` exports nothing, and that a missing `.env` is not an error.
`cargo clippy --all-targets -- -D warnings` clean. `cargo fmt --check` clean.

**Same-failure search**: the single-directory `available_services` call was
checked across every caller - `llm::propose` was the last one still reading only
`specs/`; the console's create and describe paths were already fixed in
`SOW-0006`. The "status is not an acknowledgement" class from `SOW-0005` recurs
here in a new form - a `200 OK` naming a different model than the one requested
- and is now surfaced rather than swallowed.

**Sensitive data gate**: no key value in this SOW, in any commit, in any error
message, or in debug output. `.env` is gitignored and was verified absent from
`git status`. The key travels to curl on stdin, never argv.

**Artifact maintenance gate**:

- `AGENTS.md` - no change needed; no new project-wide guardrail. The
  measure-before-trusting rule it already states is exactly what this SOW
  exercised.
- Runtime project skills - none yet; `SOW-0007` tracks the first one. This SOW
  adds a second candidate rule to it: probe a provider with the *real* payload.
- Specs - `authoring-environments.md` gains the provider, the measured model
  table and the `.env` key path.
- End-user docs - `README.md` and `docs/QUICKSTART.md` updated.
- SOW lifecycle - completed, in `done/`, committed with the work.

## Outcome

`--llm netdata` works end to end from both the CLI and the console, with no
external vendor key. The question asked was whether `deepseek-v4-flash` suits
this use-case: it does not, because the gateway does not serve it and the model
that answers in its place cannot produce structured output. `k3` can, is the
fastest of the three, and produces plans good enough that a 26-node fleet it
designed passed the fidelity lint unedited.

## Lessons Extracted

- **Test the payload, not the wiring.** A five-value schema succeeded where the
  real 262-value one failed, on the same endpoint, same model. A smoke test that
  proves the plumbing proves nothing about the request that matters.
- **A gateway's model name is a request, not a guarantee.** `deepseek-v4-flash`
  returned `MiniMax-M3` with a 200. Any integration that trusts the model field
  it sent, rather than the one it got back, is reporting fiction.
- **A model giving a right answer to the wrong question reads as a weak model.**
  It correctly said HAProxy was unmodellable, because we had handed it a
  five-item vocabulary. The instinct was to blame the model; the bug was ours.
- **A compiler guardrail is worth more than the convenience it blocks.**
  `forbid(unsafe_code)` rejected loading `.env` into the process environment and
  forced an explicit, narrower design.

## Followup

- `SOW-0007` (integration-sync project skill) gains a second rule: probe an
  external provider with the real request payload, and assert on the model that
  answered.
- The model roster on `llm.netdata.cloud` will change. The default is a
  constant with a comment and a test asserting it; `--llm-model` is the escape
  hatch. No separate SOW - re-measuring is a five-minute task documented in the
  spec table.
- `SOW-0003`, `SOW-0004`, `SOW-0008` unchanged.

## Regression Log

None yet.
