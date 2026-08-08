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
- **`--llm anthropic|openai`**: a real model, for descriptions written in the
  prospect's vocabulary rather than ours ("checkout tier fronted by an ALB, an
  Aurora writer, two ElastiCache nodes").

Both produce a `Reading`, and a `Reading` is all `render()` accepts.

### The model returns a plan, never YAML

It chooses among roles from the same table the keyword parser matches, and
services present in `specs/` on disk — enforced as JSON-schema enums *and*
re-validated on our side. So a misreading yields a wrong-but-visible fleet the
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

### API key handling

Read from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` (or `--llm-key-env`) and passed
to `curl` on **stdin**, never argv — argv is world-readable via `ps` for the
life of the process. `$ANTHROPIC_BASE_URL` / `$OPENAI_BASE_URL` point at an
internal gateway.

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
