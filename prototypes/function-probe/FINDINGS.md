# Function probe — verification results

Answers the one question that decides how Live-tab functions can be simulated for
vnodes, before any design is committed. Everything below was executed against a
live agent, not reasoned about.

- **Agent:** netdata `v2.10.0-1053-nightly`, standalone, `localhost:19999`
- **Probe:** `infra-sim-fnprobe.plugin` — plain Python, installed to
  `/etc/netdata/custom-plugins.d/`, picked up by the normal 1m plugin scan
- **Run log:** `probe-run.log` (verbatim, as the probe wrote it)
- **Source tree consulted:** `netdata/netdata @ 91c8b3741f09`

Setup: two vnodes. `sim-fnprobe-a` registers `fnprobe-shared` **and**
`fnprobe-only-a`; `sim-fnprobe-b` registers `fnprobe-shared` only — the same name
on a second host. Every byte arriving on stdin was logged.

---

## Q1 — Can a vnode carry its own functions? **YES**

**Source evidence**

- `src/plugins.d/pluginsd_functions.c:337` — the `FUNCTION` handler resolves its
  target with `pluginsd_require_scope_host()`.
- `src/plugins.d/pluginsd_internals.h:19-26` — that returns `parser->user.host`,
  i.e. the host set by the most recent `HOST` line. Not localhost.

**Empirical evidence** — both functions appeared on the vnodes, not on `laptop`:

```json
{"functions":[{"name":"fnprobe-shared","help":"probe: which host am I?","tags":"top"},
              {"name":"fnprobe-only-a","help":"probe: which host am I?","tags":"top"}]}
```

## Q2 — Does `GLOBAL` mean agent-wide? **NO — it means "not chart-scoped"**

Worth stating because every function on a stock agent is flagged `GLOBAL`, which
reads like "one per agent" and would have killed the design on sight.

`src/plugins.d/pluginsd_functions.c:340`:

```c
RRDSET *st = (global)? NULL: pluginsd_require_scope_chart(...);
```

`GLOBAL` selects host-level registration instead of chart-level. Both are scoped
to the current `HOST`.

## Q3 — Does the call tell the plugin which host was asked? **NO**

This is the load-bearing finding.

**Source evidence** — `src/plugins.d/pluginsd_functions.c:42-49` formats the call
as `FUNCTION <transaction> <timeout> "<name>" "<access>" "<source>"`. No host
field exists in the format string.

**Empirical evidence** — `fnprobe-shared` called on vnode **a**, then on vnode
**b**. The two lines the plugin received are byte-identical apart from the
transaction UUID:

```text
FUNCTION c9d3a5b0ee6743cbabefc7dd2359a1a6 60 "fnprobe-shared" "0x7ff" "method=god,role=god,permissions=0x7ff,modelcontextprotocol"
FUNCTION 41ffe92feefb49faae309e4ce2383aa6 60 "fnprobe-shared" "0x7ff" "method=god,role=god,permissions=0x7ff,modelcontextprotocol"
```

`source` is caller metadata — method, role, permissions, user, ip — and carries
nothing about the target host. `access` arrives as a hex bitmask (`0x7ff`), not
the string the plugin declared.

**Consequence for the design:** a function name shared across vnodes is
unanswerable. To serve per-node data, **the registered function name must encode
the node**. Confirmed by the control case: `fnprobe-only-a`, being unique, was
unambiguous.

## Q4 — The caller asks for `<name> info` first (unanticipated)

Not found by source reading; the probe surfaced it. Before the real call, the
caller issues a **separate** function call with ` info` appended:

```text
FUNCTION 28bfcb5c8b4d4ea48b9f49b149d65caf 10 "fnprobe-shared info" "0x7ff" "...user=mcp-tools-execute-function-registry..."
```

Note the shorter timeout (10s vs 60s). A real implementation must recognise the
` info` suffix and answer with the function's parameter schema; the probe replied
with its data table and the caller tolerated it, but that is not the contract.
Anything matching on the exact function name will silently mis-handle `info`.

## Q5 — Is the simple-table contract really that small? **YES**

The probe's whole response body was five fields, and it rendered through the API
unchanged:

```json
{"status":200,"type":"table","has_history":false,
 "columns":{"function":{"index":0,"name":"Function","type":"string","unique_key":true}},
 "data":[["fnprobe-shared","..."]]}
```

Reply framing that worked, written on stdout:

```text
FUNCTION_RESULT_BEGIN <transaction> 200 application/json <expires-epoch>
<json>
FUNCTION_RESULT_END
```

Contract reference: `src/plugins.d/FUNCTION_UI_DEVELOPER_GUIDE.md:24-90`, which
also states that for simple tables the **frontend** does filtering, search, facet
counting and sorting — the plugin returns raw rows only.

---

## Implications carried into implementation

1. Per-vnode functions need per-vnode names. There is no alternative within this
   protocol.
2. The plugin must read its own stdin. `crates/sim-plugin` currently does not
   (`logs_runtime.rs` handles only the journal child's stdin), and once functions
   are declared the agent writes calls into a pipe nobody drains.
3. Stdout needs one serialised writer. A `FUNCTION_RESULT` emitted between a
   chart's `BEGIN` and `END` corrupts the metric stream. The probe used a lock
   around every write for exactly this reason; netdata's own plugins use
   `src/libnetdata/functions_evloop/`.
4. Handle `<name> info` explicitly.
5. Do not implement log-explorer-style functions. Feeding the real store
   (journald, OTLP) already yields the real faceted UI, which is Part 2 of the
   developer guide and far more work to fake.

## Side effect left behind

The two probe vnodes (`sim-fnprobe-a`, `sim-fnprobe-b`) remain registered in the
local agent as stale nodes; their GUIDs are recorded in the probe source. Nothing
removes a vnode from an agent's database short of dropping its dbengine files, so
they were left alone rather than tampering with the operator's agent.
