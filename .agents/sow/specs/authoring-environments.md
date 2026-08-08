# Authoring environments

The three ways an `environment.yaml` comes into existence, and the rules they
all obey. Describes current reality as of `SOW-0001`.

## What an environment is

A fleet: seed, generator spec path, and per-node identity (`guid`, `hostname`,
`role`, `services`), hardware attributes, instance groups and host labels.

`seed` plus the environment reproduces the world exactly.

## 1. Hand-authored

`environments/*.yaml`. Committed templates: `web-stack`, `k8s-microservices`,
`robotics-edge`.

Committed hostnames, IPs and labels must be obviously synthetic (`sim-*`, RFC
5737 / RFC 3849 ranges) so they can never be mistaken for a real estate.

## 2. `--describe` — from a plain-text description

```bash
infra-sim --describe "3 web servers behind an nginx load balancer, \
  a postgres primary and 2 redis caches" --name acme \
  --environment environments/acme.yaml
```

Two front ends, one back end:

- **Keyword parser** (default): offline, no API key, no network.
- **`--llm netdata|anthropic|openai`**: a real model, for descriptions written
  in the prospect's vocabulary rather than ours ("checkout tier fronted by an
  ALB, an Aurora writer, two ElastiCache nodes"). `netdata` is Netdata's own
  gateway at `llm.netdata.cloud`, OpenAI-compatible on the wire.

Both produce a `Reading`, and a `Reading` is all `render()` accepts.

### The model returns a plan, never YAML

It chooses among roles from the same table the keyword parser matches, and
services present in `specs/` **and `specs/generated/`** on disk — enforced as
JSON-schema enums *and* re-validated on our side. Offering only the six
hand-authored specs is why a model asked about HAProxy reported it as
unmodellable while the offline keyword reader resolved it. So a misreading yields a wrong-but-visible fleet the
SE corrects, not an environment naming a signal no generator defines, which
fails silently mid-demo.

Anything unmappable is reported, never substituted. `spec.md` allows this: the
non-goal is per-datapoint LLM generation, and authoring a file offline is not
inference in the data path.

### Rules both paths obey

- **GUIDs are derived** from `(environment name, hostname)`, so regenerating
  reproduces the same identities instead of orphaning a running fleet.
- **`--name` is authoritative** over any model-suggested name, because the name
  fixes the seed, the hostname prefix and therefore every GUID.
- **Groups sharing a hostname element are merged** — two groups with one slug
  would emit the same hostname twice, and since the GUID derives from the
  hostname, that is two nodes claiming one identity.
- **Fillable mounts keep headroom.** A mount a hero scenario fills starts at
  ~11% utilisation, not the default 38%: `disk-fill`'s 8.2x from 38% reaches
  311%, clamps at 100%, and the ramp flattens. The lint cannot catch this —
  it does not run scenarios.

### Not every model can do this

The plan contract depends on the provider honouring a **strict `json_schema`
response format**. That is not a given. Measured against `llm.netdata.cloud` on
a real describe request (262-service enum, ~7.7k-character system prompt):

| Model asked for | Actually answered | Structured output | Time |
|---|---|---|---|
| `k3` | `k3` | honoured | 10-24s |
| `glm-5.2-max` | `glm-5.2-max` | **ignored** — prose | ~24s |
| `deepseek-v4-flash` | **`MiniMax-M3`** | **ignored** — prose | ~24s |

Two lessons encoded in the code:

- `k3` is the default for this provider, and the comment says why.
- **A gateway may answer as a different model than the one asked for**, so the
  JSON-parse failure names the model that actually replied. Without that, an
  operator sees "not valid JSON" from a model they never selected.

The same request succeeded with a 5-value enum and a short prompt, so this is a
property of the full request, not of structured output in general — which is
exactly why it has to be measured against the real payload.

### API key handling

Read from `LLM_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` (or
`--llm-key-env`), falling back to a **gitignored `.env` beside the repo**, and
passed to `curl` on **stdin**, never argv — argv is world-readable via `ps` for
the life of the process. `$NETDATA_LLM_BASE_URL` / `$ANTHROPIC_BASE_URL` /
`$OPENAI_BASE_URL` point at an internal gateway.

The `.env` fallback exists because the console runs under `sudo`, which strips
the caller's environment. Nothing is written back into the process environment:
`set_var` is unsafe, this crate forbids unsafe, and `environ` is readable by
anything that can see the process.

`curl` rather than an HTTP crate: every in-process Rust TLS stack needs a C
toolchain or cmake, and `infra-sim` is the binary that ships in the runtime
image.

A failed `--llm` call **fails the command**. Silently falling back to the
keyword parser would hand the SE a weaker reading with no way to tell.

## 3. `--reskin` — retarget a warm environment

```bash
infra-sim --reskin --from-prefix sim- --to-prefix acme- [--new-name N] [--label k=v]
```

Rewrites hostnames and labels, **never GUIDs**, so the fleet keeps its history,
trained ML models and alert log — turning a cold 72-hour warm-up into a change
measured in minutes.

Operates on YAML text rather than parse-and-reserialise, so comments survive: an
environment file carries authored explanation the next person needs.

Refuses to write if any GUID changed, or if the result would duplicate GUIDs
already used by a sibling environment.

## Warm-up

Netdata ML needs roughly 72 hours to be fully credible
(`training window=6h`, `train every=3h`, 18 models/dimension, delete after 7d).
Anomaly detection starts contributing after ~15 minutes.

## Measured timings

Description → live fleet: **~4m11s cold** (125s lint + 124s release build + ~2s
to node visibility), **~30s warm**.
