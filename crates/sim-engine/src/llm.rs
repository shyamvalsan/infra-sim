//! Optional LLM backing for `--describe`.
//!
//! The keyword parser in [`crate::describe`] reads our vocabulary. A prospect
//! writes in theirs — "checkout tier fronted by an ALB, an Aurora writer, two
//! ElastiCache nodes" — and that is where a model earns its place.
//!
//! **The model returns a plan, never YAML.** It picks from a catalogue of roles
//! and service specs that actually exist on disk, says how many of each, and
//! names them; [`crate::describe::render`] does the rest. A model that
//! misreads therefore produces a wrong-but-valid fleet the SE can see and fix,
//! rather than an environment referencing a signal no generator defines — which
//! would fail silently, mid-demo, with nothing in any log to explain it. The
//! same boundary keeps GUIDs derived from hostnames, so regenerating an
//! environment never orphans a running fleet's history.
//!
//! This is authoring-time only. `spec.md`'s non-goal is per-datapoint LLM
//! generation; the runtime stays cheap deterministic code with no inference
//! anywhere in the data path.
//!
//! ## Why `curl` and not an HTTP crate
//!
//! Every in-process TLS stack for Rust (rustls via `ring` or `aws-lc-rs`,
//! native-tls via OpenSSL) needs a C toolchain or cmake to build. This project
//! is meant to be cloned and built on an SE's laptop, and `infra-sim` is the
//! binary that ships in the runtime image — taking a TLS stack and ~100 crates
//! into the data-path binary to support one authoring-time call is a bad trade.
//! `curl` is present everywhere this runs, honours the operator's existing
//! proxy configuration for free, and costs the build nothing.
//!
//! The API key is written to curl's **stdin** as a config file, never to argv
//! and never to disk. Anything in argv is world-readable via `ps` for as long
//! as the process lives.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use std::collections::BTreeMap;

use crate::describe::{available_services, roles, Group, Reading};

/// Which API to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    /// Netdata's own inference gateway, `llm.netdata.cloud`. OpenAI-compatible
    /// on the wire, so it shares every request and response branch below - only
    /// the host, key and default model differ.
    Netdata,
}

impl Provider {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Ok(Provider::Anthropic),
            "openai" | "gpt" => Ok(Provider::OpenAi),
            "netdata" | "llm.netdata.cloud" => Ok(Provider::Netdata),
            other => Err(format!(
                "unknown --llm provider '{other}'; expected 'netdata', 'anthropic' or 'openai'"
            )),
        }
    }

    /// Environment variable holding the key, when not overridden.
    pub fn default_key_env(self) -> &'static str {
        match self {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::Netdata => "LLM_API_KEY",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5",
            // OpenAI's naming moves faster than this file will; `--llm-model`
            // is the escape hatch and the provider's own 404 names the problem
            // precisely when this default goes stale.
            Provider::OpenAi => "gpt-5",
            // Measured, not assumed. The whole plan contract depends on the
            // provider honouring a strict `json_schema` response format, and on
            // this gateway that is not a given:
            //
            //   k3                 obeys the schema, 10-24s
            //   glm-5.2-max        returns free-form reasoning, schema ignored
            //   deepseek-v4-flash  is not served at all - the gateway answers
            //                      with MiniMax-M3, which ignores the schema
            //
            // Override with --llm-model when the roster changes.
            Provider::Netdata => "k3",
        }
    }

    /// `(base-url environment variable, default host, request path)`.
    ///
    /// The base URL is overridable under the same variable names the official
    /// SDKs read, because plenty of enterprises route model traffic through an
    /// internal gateway rather than letting a laptop reach the vendor directly.
    fn endpoint(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Provider::Anthropic => (
                "ANTHROPIC_BASE_URL",
                "https://api.anthropic.com",
                "/v1/messages",
            ),
            Provider::OpenAi => (
                "OPENAI_BASE_URL",
                "https://api.openai.com",
                "/v1/chat/completions",
            ),
            Provider::Netdata => (
                "NETDATA_LLM_BASE_URL",
                "https://llm.netdata.cloud",
                "/v1/chat/completions",
            ),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Netdata => "netdata",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub key_env: String,
    /// Repository root, searched for a `.env` when the variable is unset.
    pub repo: Option<std::path::PathBuf>,
    /// Client-side deadline. Generous: a thinking model on a gnarly description
    /// is slow, and a spurious timeout looks exactly like an outage.
    pub timeout_secs: u64,
}

impl Config {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            model: provider.default_model().to_string(),
            key_env: provider.default_key_env().to_string(),
            repo: None,
            timeout_secs: 240,
        }
    }

    /// Full request URL, after any base-URL override.
    fn url(&self) -> String {
        let (var, default, path) = self.provider.endpoint();
        let base = std::env::var(var)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default.to_string());
        join_url(&base, path)
    }
}

/// Where a provider key may be found, besides the environment.
///
/// The key has to reach the process somehow, and the alternatives are worse: a
/// config file inside the repo is a credential one `git add` from being
/// published, and typing it into the console's web form puts it in the DOM and
/// in browser history. A gitignored `.env` beside the repo is read here and
/// never logged.
///
/// Nothing is written back into the process environment - `set_var` is unsafe,
/// this crate forbids unsafe, and a value in `environ` is readable by anything
/// that can see the process anyway.
pub fn env_file(repo: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(repo.join(".env")) else {
        return out;
    };
    // Deliberately minimal: no interpolation, no `export` prefix, no multi-line
    // values. A .env needing those is doing more than holding one API key.
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !key.is_empty() && !value.is_empty() {
                out.insert(key.to_string(), value.to_string());
            }
        }
    }
    out
}

/// Every service spec that can actually be composed onto a node.
///
/// The plan schema pins `services` to this list as a JSON-schema enum, so it is
/// also the model's entire vocabulary - anything missing here is something the
/// model is structurally unable to propose.
pub fn installable_services(specs_dir: &Path) -> Vec<String> {
    let mut all = available_services(specs_dir);
    all.extend(available_services(&specs_dir.join("generated")));
    all.sort();
    all.dedup();
    all
}

/// Providers whose key is resolvable right now, for the console's picker.
///
/// A provider that would fail on the first request is not a choice worth
/// offering.
pub fn available(repo: &Path) -> Vec<Provider> {
    let file = env_file(repo);
    [Provider::Netdata, Provider::Anthropic, Provider::OpenAi]
        .into_iter()
        .filter(|p| {
            let var = p.default_key_env();
            std::env::var(var)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
                || file.contains_key(var)
        })
        .collect()
}

/// Join a base URL to a request path, tolerating a trailing slash on the base.
fn join_url(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim().trim_end_matches('/'))
}

/// What the model proposed, after validation against the catalogue.
#[derive(Debug)]
pub struct Proposal {
    pub reading: Reading,
    /// The model's own account of how it read the description.
    pub notes: Vec<String>,
    /// Things it was asked for that no generator spec can model.
    pub unsupported: Vec<String>,
    /// Suggested environment name, used only when `--name` was not given.
    pub suggested_name: Option<String>,
    /// Model that actually answered, as reported by the provider.
    pub model: String,
    /// Adjustments made to the model's plan during validation, shown to the SE
    /// so a silently corrected plan is not mistaken for an accepted one.
    pub corrections: Vec<String>,
}

/// The model name the provider actually answered with.
///
/// A gateway may alias or fall back, so the model that replied is not
/// necessarily the model that was asked for - and when the substitute does not
/// honour structured output, the failure surfaces as unparseable JSON with no
/// hint of why.
fn served_model(raw: &Value) -> Option<String> {
    raw.get("model")?.as_str().map(str::to_string)
}

/// Ask a model to map a description onto the catalogue.
pub fn propose(cfg: &Config, description: &str, specs_dir: &Path) -> Result<Proposal, String> {
    let key = std::env::var(&cfg.key_env)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| {
            cfg.repo
                .as_deref()
                .and_then(|repo| env_file(repo).remove(&cfg.key_env))
        })
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            format!(
                "no key for {}: ${} is not set and no `.env` beside the repo defines it.\n\
                 Add it to .env (which is gitignored) or export it, or drop --llm to use \
                 the offline keyword parser:\n  echo '{}=...' >> .env",
                cfg.provider.label(),
                cfg.key_env,
                cfg.key_env
            )
        })?;

    // Both directories: the six hand-authored specs and the ~250 synced from
    // Netdata's collector metadata. Offering only the former is why a model
    // asked about HAProxy, RabbitMQ and Elasticsearch reported them as "not
    // modelled" while the offline keyword reader resolved all three.
    let services = installable_services(specs_dir);
    if services.is_empty() {
        return Err(format!(
            "no service specs found under '{}'; nothing to compose onto a node",
            specs_dir.display()
        ));
    }

    let system = system_prompt(&services);
    let schema = plan_schema(&services);
    let body = request_body(cfg, &system, description, &schema);

    let raw = call(cfg, &key, &body)?;
    // Set INFRA_SIM_LLM_DEBUG=1 to see exactly what a provider returned. A
    // plan that reads as nonsense is otherwise indistinguishable from a bug in
    // the validation below, and the response never contains the key.
    if std::env::var("INFRA_SIM_LLM_DEBUG").is_ok_and(|v| v == "1") {
        eprintln!("--- request ---\n{body}\n--- response ---\n{raw}\n---");
    }
    let (text, model) = match cfg.provider {
        Provider::Anthropic => anthropic_text(&raw)?,
        Provider::OpenAi | Provider::Netdata => openai_text(&raw)?,
    };

    let plan: Value = serde_json::from_str(&text).map_err(|e| {
        // Almost always means the model ignored the strict `json_schema`
        // response format and answered in prose. Naming the model that actually
        // replied is the point: a gateway that aliases or falls back will
        // answer as something else entirely, and without this the operator sees
        // only "not valid JSON" from a model they never chose.
        let served = served_model(&raw).unwrap_or_else(|| "unknown".into());
        let aliased = if served != cfg.model {
            format!(
                "\nAsked for '{}' but '{served}' answered - this gateway aliases or falls \
                 back, and the substitute does not honour structured output. Pick a model \
                 that does with --llm-model.",
                cfg.model
            )
        } else {
            format!(
                "\n'{served}' does not honour a strict json_schema response format on a \
                 request this size. Pick a model that does with --llm-model."
            )
        };
        format!(
            "the model's reply was not valid JSON ({e}).{aliased}\n\
             First 400 characters:\n{}",
            text.chars().take(400).collect::<String>()
        )
    })?;

    let mut proposal = validate(&plan, &services)?;
    proposal.model = model;
    reinstate_droppped_software(&mut proposal, description, &services);
    Ok(proposal)
}

/// System prompt.
///
/// Written as context rather than commands: current models follow a plainly
/// stated constraint, and shouted emphasis on every line stops carrying
/// information. The one thing worth stating twice is the failure mode, because
/// a plausible substitution is worse here than an admitted gap.
fn system_prompt(services: &[String]) -> String {
    let mut s = String::new();
    s.push_str(
        "You turn a plain-text description of someone's infrastructure into a plan for a \
         synthetic monitoring fleet. The plan is rendered into a configuration file by code \
         that only accepts the roles and services listed below, so a plan naming anything \
         else is rejected rather than approximated.\n\n\
         Available roles:\n",
    );
    for r in roles() {
        s.push_str(&format!(
            "  {:<18} default services: {:<22} hostname element: {:<10} {}\n",
            r.role,
            if r.services.is_empty() {
                "(none)".to_string()
            } else {
                r.services.join(", ")
            },
            r.slug,
            r.summary
        ));
    }
    s.push_str(&format!(
        "\nAvailable service specs (a node may take any combination):\n  {}\n",
        services.join(", ")
    ));
    s.push_str(
        "\nHow to read a description:\n\
         - Map each part of the stack onto the closest role. Managed and cloud-branded \
           products map to the role whose shape they share: a hosted relational database is \
           'db', a hosted cache is 'cache', a cloud load balancer is 'lb'.\n\
         - A role is a node *shape* - how much CPU, RAM and disk, and which scenarios can \
           target it - not a category of software. If a named component has a service spec \
           in the list above, it MUST appear in `groups` on the closest-shaped role, with \
           that spec in `services`. Elasticsearch, Kafka, RabbitMQ and the like are ordinary \
           servers: give them `web` unless the description says otherwise. Dropping software \
           that has a spec is the single worst error you can make here - the prospect looks \
           for their service and it is simply absent.\n\
         - `unsupported` is only for components with NO spec in the list above. Name the \
           component and say plainly that no spec exists. Never put something there because \
           no role sounds like a category match.\n\
         - `count` is how many nodes of that group to create. When the description implies \
           a tier without a number, one or two is a safer reading than a guess at scale; \
           say so in `notes`.\n\
         - `slug` becomes the hostname element, as in acme-<slug>-01. Prefer the \
           prospect's own word for the tier ('checkout', 'catalog') over the generic role \
           name when the description offers one; otherwise use the role's own element. Use \
           lowercase letters, digits and hyphens. Give each group a distinct slug: groups \
           sharing one are merged into a single tier.\n\
         - `services` defaults to the role's, and is worth changing only when the \
           description names something different that is on the list above.\n\
         - `source` is the phrase from the description this group came from, quoted back so \
           the reading can be checked.\n\
         - `notes` is where assumptions belong - anything you inferred, sized by guess, or \
           read one of two ways.\n\
         - `environment_name` is a short lowercase identifier for the fleet, usually the \
           company or project name if the description offers one.\n",
    );
    s
}

/// JSON schema for the plan.
///
/// Roles and services are enums rather than free strings, so the constraint is
/// enforced by the provider before the reply is even returned. It is checked
/// again on this side: a schema is a contract, not a guarantee.
fn plan_schema(services: &[String]) -> Value {
    let role_names: Vec<&str> = roles().iter().map(|r| r.role).collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["groups", "notes", "unsupported", "environment_name"],
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["role", "count", "services", "slug", "source"],
                    "properties": {
                        "role": { "type": "string", "enum": role_names },
                        "count": { "type": "integer" },
                        "services": {
                            "type": "array",
                            "items": { "type": "string", "enum": services }
                        },
                        "slug": { "type": "string" },
                        "source": { "type": "string" }
                    }
                }
            },
            "notes": { "type": "array", "items": { "type": "string" } },
            "unsupported": { "type": "array", "items": { "type": "string" } },
            "environment_name": { "type": "string" }
        }
    })
}

fn request_body(cfg: &Config, system: &str, description: &str, schema: &Value) -> String {
    let body = match cfg.provider {
        Provider::Anthropic => json!({
            "model": cfg.model,
            // Caps thinking plus response together. Thinking is on by default
            // on current models, so a budget sized only for the plan truncates.
            "max_tokens": 16000,
            "system": system,
            "messages": [{ "role": "user", "content": description }],
            "output_config": {
                // Mapping a description onto a fixed catalogue is constrained
                // work, and an SE is waiting at a terminal. Raise this if a
                // description is genuinely ambiguous and the reading suffers.
                "effort": "medium",
                "format": { "type": "json_schema", "schema": schema }
            }
        }),
        Provider::OpenAi | Provider::Netdata => json!({
            "model": cfg.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": description }
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "fleet_plan", "strict": true, "schema": schema }
            }
        }),
    };
    body.to_string()
}

/// A temp file removed when it goes out of scope, including on the error paths.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// POST the request and return the parsed response body.
fn call(cfg: &Config, key: &str, body: &str) -> Result<Value, String> {
    // The request body goes in a file so stdin is free for the config that
    // carries the key. The body is not secret, but it can name a prospect, so
    // it is written owner-only and deleted on the way out.
    let path = std::env::temp_dir().join(format!(
        "infra-sim-llm-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let scratch = Scratch(path.clone());
    std::fs::write(&path, body).map_err(|e| format!("cannot write request body: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let mut child = Command::new("curl")
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg(cfg.timeout_secs.to_string())
        // The status code is appended after the body so a 4xx still yields the
        // provider's own error message instead of an opaque exit code.
        .arg("--write-out")
        .arg("\n%{http_code}")
        .arg("--config")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run curl: {e}.\n\
                 --llm calls the {} API over curl; install curl, or drop --llm to use the \
                 offline keyword parser.",
                cfg.provider.label()
            )
        })?;

    let config = curl_config(cfg, key, &path);
    child
        .stdin
        .take()
        .ok_or_else(|| "curl stdin unavailable".to_string())?
        .write_all(config.as_bytes())
        .map_err(|e| format!("cannot pass request options to curl: {e}"))?;

    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    drop(scratch);

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "curl could not reach {}: {}",
            cfg.url(),
            if stderr.trim().is_empty() {
                format!("exit status {}", out.status)
            } else {
                stderr.trim().to_string()
            }
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let (payload, status) = stdout
        .trim_end()
        .rsplit_once('\n')
        .ok_or_else(|| format!("unreadable response from {}", cfg.url()))?;
    let status: u16 = status.trim().parse().unwrap_or(0);

    let parsed: Value = serde_json::from_str(payload)
        .map_err(|e| format!("HTTP {status} from {}: {e}", cfg.url()))?;

    if status != 200 {
        // Both providers nest a human-readable message under "error"; fall back
        // to the raw payload so nothing is swallowed.
        let message = parsed
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or(payload);
        return Err(format!(
            "HTTP {status} from {}: {message}",
            cfg.provider.label()
        ));
    }

    Ok(parsed)
}

/// Build curl's config file. This carries the key, so it goes to stdin only.
fn curl_config(cfg: &Config, key: &str, body_path: &Path) -> String {
    let mut lines = vec![
        "request = \"POST\"".to_string(),
        format!("url = {}", quote(&cfg.url())),
        format!("header = {}", quote("content-type: application/json")),
    ];
    match cfg.provider {
        Provider::Anthropic => {
            lines.push(format!(
                "header = {}",
                quote("anthropic-version: 2023-06-01")
            ));
            lines.push(format!("header = {}", quote(&format!("x-api-key: {key}"))));
        }
        Provider::OpenAi | Provider::Netdata => {
            lines.push(format!(
                "header = {}",
                quote(&format!("authorization: Bearer {key}"))
            ));
        }
    }
    lines.push(format!(
        "data-binary = {}",
        quote(&format!("@{}", body_path.display()))
    ));
    lines.join("\n") + "\n"
}

/// Quote a curl config value. Keys are alphanumeric in practice, but a value
/// that broke out of its quotes would put credentials somewhere unexpected.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pull the JSON plan out of an Anthropic response.
fn anthropic_text(body: &Value) -> Result<(String, String), String> {
    let stop = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    if stop == "refusal" {
        let category = body
            .get("stop_details")
            .and_then(|d| d.get("category"))
            .and_then(Value::as_str)
            .unwrap_or("unspecified");
        return Err(format!(
            "the model declined this request (category: {category}).\n\
             Drop --llm to use the offline keyword parser."
        ));
    }
    if stop == "max_tokens" {
        return Err(
            "the model ran out of output budget before finishing the plan. Try a shorter \
             description, or split the fleet across two runs."
                .into(),
        );
    }

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    // Never index content[0]: thinking blocks precede the text block, and the
    // set of block types grows over time.
    let text = body
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|b| b.get("text"))
                .and_then(Value::as_str)
        })
        .ok_or_else(|| "the response contained no text block".to_string())?;

    Ok((text.to_string(), model))
}

/// Pull the JSON plan out of an OpenAI chat completion.
fn openai_text(body: &Value) -> Result<(String, String), String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| "the response contained no choices".to_string())?;

    if let Some(refusal) = choice
        .get("message")
        .and_then(|m| m.get("refusal"))
        .and_then(Value::as_str)
    {
        return Err(format!(
            "the model declined this request: {refusal}\n\
             Drop --llm to use the offline keyword parser."
        ));
    }
    if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        return Err(
            "the model ran out of output budget before finishing the plan. Try a shorter \
             description."
                .into(),
        );
    }

    let text = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| "the response contained no message content".to_string())?;

    Ok((text.to_string(), model))
}

/// Check a plan against the catalogue and turn it into a [`Reading`].
///
/// The schema already constrains roles and services, so most of this never
/// fires — but it is what makes the guarantee hold rather than merely be
/// requested, and a provider that ignores an enum should be caught here rather
/// than by a fleet that emits nothing.
fn validate(plan: &Value, services: &[String]) -> Result<Proposal, String> {
    let known_roles: Vec<&str> = roles().iter().map(|r| r.role).collect();
    let mut corrections = Vec::new();
    let mut groups = Vec::new();

    let raw_groups = plan
        .get("groups")
        .and_then(Value::as_array)
        .ok_or_else(|| "the plan had no 'groups' array".to_string())?;

    for (i, g) in raw_groups.iter().enumerate() {
        let role = g.get("role").and_then(Value::as_str).unwrap_or_default();
        if !known_roles.contains(&role) {
            corrections.push(format!(
                "dropped group {i}: role '{role}' is not one this project can model"
            ));
            continue;
        }

        let count = g.get("count").and_then(Value::as_i64).unwrap_or(1);
        let clamped = count.clamp(1, 500) as usize;
        if clamped as i64 != count {
            corrections.push(format!(
                "group {i} ({role}): count {count} out of range, using {clamped}"
            ));
        }

        let mut kept = Vec::new();
        if let Some(list) = g.get("services").and_then(Value::as_array) {
            for s in list.iter().filter_map(Value::as_str) {
                if services.iter().any(|k| k == s) {
                    kept.push(s.to_string());
                } else {
                    corrections.push(format!(
                        "group {i} ({role}): dropped service '{s}', no spec for it on disk"
                    ));
                }
            }
        }
        if kept.is_empty() {
            // A base-only node is a legitimate outcome, but silently losing the
            // service the SE asked about is not, so fall back to the role's own.
            let defaults: Vec<String> = roles()
                .iter()
                .find(|r| r.role == role)
                .map(|r| r.services.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            kept = defaults
                .into_iter()
                .filter(|d| services.contains(d))
                .collect();
        }

        let slug = g
            .get("slug")
            .and_then(Value::as_str)
            .map(sanitise_slug)
            .filter(|s| !s.is_empty());

        groups.push(Group {
            count: clamped,
            role: role.to_string(),
            services: kept,
            slug,
            source: g
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }

    if groups.is_empty() {
        return Err(format!(
            "the model produced no usable groups.\n{}",
            string_list(plan, "unsupported")
                .into_iter()
                .map(|u| format!("  it reported as unsupported: {u}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let before = groups.len();
    let mut reading = Reading {
        groups,
        unrecognised: Vec::new(),
    };
    reading.dedupe_slugs();
    if reading.groups.len() != before {
        corrections.push(format!(
            "merged {} group(s) that shared a hostname element",
            before - reading.groups.len()
        ));
    }

    let suggested_name = plan
        .get("environment_name")
        .and_then(Value::as_str)
        .map(sanitise_slug)
        .filter(|s| !s.is_empty());

    Ok(Proposal {
        reading,
        notes: string_list(plan, "notes"),
        unsupported: string_list(plan, "unsupported"),
        suggested_name,
        model: String::new(),
        corrections,
    })
}

/// Put back software the model discarded even though a spec for it exists.
///
/// The offline keyword reader is a floor the model is not allowed to fall
/// below: it resolves any catalogue id named in the text, deterministically. If
/// the model dropped one into `unsupported`, that is simply wrong - the spec is
/// right there - and the SE would find the prospect's service missing from the
/// fleet with only a line of prose to explain it.
///
/// This does not second-guess the model's *judgement*, only its bookkeeping: a
/// group is reinstated solely for software the deterministic reader found in the
/// same sentence and the model failed to place anywhere.
fn reinstate_droppped_software(proposal: &mut Proposal, description: &str, services: &[String]) {
    if proposal.unsupported.is_empty() {
        return;
    }
    let placed: std::collections::BTreeSet<String> = proposal
        .reading
        .groups
        .iter()
        .flat_map(|g| g.services.iter().cloned())
        .collect();

    let offline = crate::describe::parse_with_services(description, services);
    let mut reinstated: Vec<String> = Vec::new();

    for group in offline.groups {
        let missing: Vec<String> = group
            .services
            .iter()
            .filter(|svc| !placed.contains(*svc))
            // Only what the model actually claimed it could not model, so a
            // group it deliberately left out for another reason is untouched.
            .filter(|svc| {
                proposal
                    .unsupported
                    .iter()
                    .any(|u| u.to_lowercase().contains(svc.as_str()))
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            continue;
        }
        reinstated.extend(missing.iter().cloned());
        proposal.reading.groups.push(Group {
            services: missing,
            ..group
        });
    }

    if reinstated.is_empty() {
        return;
    }
    proposal
        .unsupported
        .retain(|u| !reinstated.iter().any(|r| u.to_lowercase().contains(r)));
    proposal.reading.dedupe_slugs();
    proposal.corrections.push(format!(
        "reinstated {} - the model reported it unsupported, but a generator spec exists",
        reinstated.join(", ")
    ));
}

fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Reduce a model-supplied name to something safe in a hostname.
///
/// Any run of non-alphanumerics becomes one hyphen rather than being dropped:
/// deleting the separator in "db/prod" yields "dbprod", which reads as a
/// different tier rather than a sanitised one.
fn sanitise_slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    // Keep these near the top so a provider added without an endpoint is caught.
    #[test]
    fn the_netdata_provider_points_at_the_gateway() {
        let cfg = super::Config::new(super::Provider::Netdata);
        assert_eq!(cfg.key_env, "LLM_API_KEY");
        // Not deepseek-v4-flash: the gateway answers that with MiniMax-M3,
        // which ignores the strict json_schema the plan contract needs.
        assert_eq!(cfg.model, "k3");
        assert_eq!(
            cfg.url(),
            "https://llm.netdata.cloud/v1/chat/completions",
            "OpenAI-compatible path"
        );
    }

    #[test]
    fn an_env_file_yields_keys_without_touching_the_environment() {
        let dir = std::env::temp_dir().join(format!("infra-sim-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "# a comment\n\nLLM_API_KEY=\"secret-value\"\nEMPTY=\nNO_EQUALS\nOTHER = plain \n",
        )
        .unwrap();
        let found = super::env_file(&dir);
        assert_eq!(
            found.get("LLM_API_KEY").map(String::as_str),
            Some("secret-value")
        );
        assert_eq!(found.get("OTHER").map(String::as_str), Some("plain"));
        assert!(!found.contains_key("EMPTY"), "an empty value is not a key");
        assert!(!found.contains_key("NO_EQUALS"));
        assert!(
            std::env::var("LLM_API_KEY").is_err() || std::env::var_os("OTHER").is_none(),
            "reading a .env must not export anything"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_env_file_is_not_an_error() {
        assert!(super::env_file(std::path::Path::new("/nonexistent-abc")).is_empty());
    }

    use super::*;

    fn services() -> Vec<String> {
        ["containers", "kubernetes", "nginx", "postgres", "redis"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn the_prompt_offers_every_role_and_service() {
        let p = system_prompt(&services());
        for r in roles() {
            assert!(p.contains(r.role), "role '{}' missing from prompt", r.role);
        }
        for s in services() {
            assert!(p.contains(&s), "service '{s}' missing from prompt");
        }
    }

    #[test]
    fn the_schema_pins_roles_and_services_to_the_catalogue() {
        // The whole safety argument rests on the model being unable to name a
        // role or service the renderer cannot honour.
        let schema = plan_schema(&services());
        let role_enum = schema["properties"]["groups"]["items"]["properties"]["role"]["enum"]
            .as_array()
            .expect("roles are an enum");
        assert_eq!(role_enum.len(), roles().len());
        let svc_enum = schema["properties"]["groups"]["items"]["properties"]["services"]["items"]
            ["enum"]
            .as_array()
            .expect("services are an enum");
        assert_eq!(svc_enum.len(), services().len());
    }

    #[test]
    fn structured_output_objects_forbid_extra_properties() {
        // Structured outputs reject a schema whose objects allow unknown keys.
        let schema = plan_schema(&services());
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["properties"]["groups"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    fn anthropic_reply(plan: Value) -> Value {
        json!({
            "id": "msg_01",
            "type": "message",
            "model": "claude-opus-5",
            "stop_reason": "end_turn",
            "content": [
                // Thinking blocks precede text; indexing content[0] would break.
                { "type": "thinking", "thinking": "", "signature": "abc" },
                { "type": "text", "text": plan.to_string() }
            ]
        })
    }

    #[test]
    fn reads_the_text_block_past_a_thinking_block() {
        let (text, model) =
            anthropic_text(&anthropic_reply(json!({ "groups": [] }))).expect("parses");
        assert_eq!(model, "claude-opus-5");
        assert!(text.contains("groups"));
    }

    #[test]
    fn a_refusal_is_reported_not_parsed_as_a_plan() {
        let body = json!({
            "model": "claude-opus-5",
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "cyber" },
            "content": []
        });
        let err = anthropic_text(&body).unwrap_err();
        assert!(err.contains("declined"), "{err}");
        assert!(err.contains("cyber"), "{err}");
    }

    #[test]
    fn a_truncated_reply_is_reported_not_half_applied() {
        let body = json!({
            "model": "claude-opus-5",
            "stop_reason": "max_tokens",
            "content": [{ "type": "text", "text": "{\"groups\": [" }]
        });
        assert!(anthropic_text(&body).unwrap_err().contains("output budget"));
    }

    #[test]
    fn reads_an_openai_completion() {
        let body = json!({
            "model": "gpt-5",
            "choices": [{
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "{\"groups\": []}" }
            }]
        });
        let (text, model) = openai_text(&body).expect("parses");
        assert_eq!(model, "gpt-5");
        assert_eq!(text, "{\"groups\": []}");
    }

    #[test]
    fn an_openai_refusal_is_reported() {
        let body = json!({
            "model": "gpt-5",
            "choices": [{ "message": { "refusal": "I can't help with that" } }]
        });
        assert!(openai_text(&body).unwrap_err().contains("declined"));
    }

    #[test]
    fn software_with_a_spec_is_never_left_unsupported() {
        // The failure this guards: the model reported "Elasticsearch for logs"
        // as having no reasonable role and dropped it, while
        // specs/generated/elasticsearch.yaml sat right there. The prospect
        // looks for their service and it is simply absent from the fleet.
        let services: Vec<String> = ["nginx", "postgres", "elasticsearch"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let plan = json!({
            "environment_name": "acme",
            "groups": [{
                "role": "web", "count": 4, "services": ["nginx"],
                "slug": "app", "source": "four app servers"
            }],
            "notes": [],
            "unsupported": ["Elasticsearch for logs (search cluster - no reasonable role)"]
        });
        let mut p = super::validate(&plan, &services).unwrap();
        super::reinstate_droppped_software(
            &mut p,
            "four app servers and an elasticsearch cluster of 3",
            &services,
        );

        let placed: Vec<&str> = p
            .reading
            .groups
            .iter()
            .flat_map(|g| g.services.iter().map(String::as_str))
            .collect();
        assert!(placed.contains(&"elasticsearch"), "placed: {placed:?}");
        assert!(
            p.unsupported.is_empty(),
            "still reported unsupported: {:?}",
            p.unsupported
        );
        assert!(p.corrections.iter().any(|c| c.contains("elasticsearch")));
        // The count came from the description, not from a guess.
        let es = p
            .reading
            .groups
            .iter()
            .find(|g| g.services.iter().any(|s| s == "elasticsearch"))
            .unwrap();
        assert_eq!(es.count, 3);
    }

    #[test]
    fn software_with_no_spec_stays_unsupported() {
        let services: Vec<String> = ["nginx"].iter().map(|s| s.to_string()).collect();
        let plan = json!({
            "environment_name": "acme",
            "groups": [{
                "role": "web", "count": 2, "services": ["nginx"],
                "slug": "app", "source": "two app servers"
            }],
            "notes": [],
            "unsupported": ["a Kafka cluster"]
        });
        let mut p = super::validate(&plan, &services).unwrap();
        super::reinstate_droppped_software(
            &mut p,
            "two app servers and a kafka cluster",
            &services,
        );
        assert_eq!(p.unsupported, vec!["a Kafka cluster"]);
        assert!(p.corrections.is_empty());
    }

    #[test]
    fn a_valid_plan_becomes_a_reading() {
        let plan = json!({
            "environment_name": "Acme Retail",
            "groups": [
                { "role": "web", "count": 3, "services": ["nginx"],
                  "slug": "checkout", "source": "checkout tier" },
                { "role": "db", "count": 1, "services": ["postgres"],
                  "slug": "aurora", "source": "Aurora writer" }
            ],
            "notes": ["assumed one writer"],
            "unsupported": ["SQS queues"]
        });
        let p = validate(&plan, &services()).expect("valid");
        assert_eq!(p.reading.groups.len(), 2);
        assert_eq!(p.reading.groups[0].effective_slug(), "checkout");
        assert_eq!(p.reading.groups[0].count, 3);
        assert_eq!(p.suggested_name.as_deref(), Some("acme-retail"));
        assert_eq!(p.unsupported, vec!["SQS queues"]);
        assert!(p.corrections.is_empty(), "{:?}", p.corrections);
    }

    #[test]
    fn a_role_outside_the_catalogue_is_dropped_not_rendered() {
        // The schema should prevent this; if a provider ignores the enum, a
        // node with an unmodellable role is an empty dashboard.
        let plan = json!({
            "groups": [
                { "role": "kafka", "count": 3, "services": [], "slug": "kafka", "source": "x" },
                { "role": "web", "count": 1, "services": ["nginx"], "slug": "web", "source": "y" }
            ]
        });
        let p = validate(&plan, &services()).unwrap();
        assert_eq!(p.reading.groups.len(), 1);
        assert_eq!(p.reading.groups[0].role, "web");
        assert!(
            p.corrections.iter().any(|c| c.contains("kafka")),
            "{:?}",
            p.corrections
        );
    }

    #[test]
    fn a_service_with_no_spec_on_disk_is_dropped() {
        let plan = json!({
            "groups": [{
                "role": "db", "count": 1, "services": ["cassandra"],
                "slug": "db", "source": "x"
            }]
        });
        let p = validate(&plan, &services()).unwrap();
        // Falls back to the role's own services rather than losing the service
        // the description was actually about.
        assert_eq!(p.reading.groups[0].services, vec!["postgres"]);
        assert!(p.corrections.iter().any(|c| c.contains("cassandra")));
    }

    #[test]
    fn groups_sharing_a_hostname_element_are_merged() {
        // Two groups with one slug would emit the same hostname twice, and the
        // GUID derives from the hostname - so both nodes claim one identity.
        let plan = json!({
            "groups": [
                { "role": "web", "count": 2, "services": ["nginx"], "slug": "app", "source": "a" },
                { "role": "web", "count": 3, "services": ["nginx"], "slug": "app", "source": "b" }
            ]
        });
        let p = validate(&plan, &services()).unwrap();
        assert_eq!(p.reading.groups.len(), 1);
        assert_eq!(p.reading.groups[0].count, 5);
        assert!(p.corrections.iter().any(|c| c.contains("hostname element")));
    }

    #[test]
    fn an_absurd_count_is_clamped_and_reported() {
        let plan = json!({
            "groups": [{
                "role": "web", "count": 100000, "services": ["nginx"],
                "slug": "web", "source": "x"
            }]
        });
        let p = validate(&plan, &services()).unwrap();
        assert_eq!(p.reading.groups[0].count, 500);
        assert!(p.corrections.iter().any(|c| c.contains("out of range")));
    }

    #[test]
    fn an_empty_plan_is_an_error_carrying_what_it_could_not_model() {
        let plan = json!({ "groups": [], "unsupported": ["a Kafka cluster"] });
        let err = validate(&plan, &services()).unwrap_err();
        assert!(err.contains("Kafka"), "{err}");
    }

    #[test]
    fn slugs_are_reduced_to_something_safe_in_a_hostname() {
        assert_eq!(sanitise_slug("Checkout Tier"), "checkout-tier");
        assert_eq!(sanitise_slug("  db//prod  "), "db-prod");
        assert_eq!(sanitise_slug("!!!"), "");
        assert_eq!(sanitise_slug("a".repeat(80).as_str()).len(), 32);
    }

    #[test]
    fn the_key_never_reaches_argv() {
        // Anything in argv is readable via ps for the life of the process.
        let cfg = Config::new(Provider::Anthropic);
        let config = curl_config(&cfg, "NOT-A-REAL-KEY", Path::new("/tmp/body.json"));
        assert!(config.contains("x-api-key: NOT-A-REAL-KEY"));
        assert!(config.contains("data-binary = \"@/tmp/body.json\""));
        assert!(config.contains("anthropic-version: 2023-06-01"));
    }

    #[test]
    fn openai_uses_a_bearer_header() {
        let cfg = Config::new(Provider::OpenAi);
        let config = curl_config(&cfg, "NOT-A-REAL-KEY", Path::new("/tmp/b.json"));
        assert!(config.contains("authorization: Bearer NOT-A-REAL-KEY"));
        assert!(!config.contains("anthropic-version"));
    }

    #[test]
    fn config_values_cannot_break_out_of_their_quotes() {
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote("a\nb"), "\"a b\"");
    }

    #[test]
    fn the_anthropic_body_carries_the_schema_and_a_token_budget() {
        let cfg = Config::new(Provider::Anthropic);
        let body: Value = serde_json::from_str(&request_body(
            &cfg,
            "sys",
            "3 web servers",
            &plan_schema(&services()),
        ))
        .unwrap();
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        // Thinking is on by default and counts against this.
        assert!(body["max_tokens"].as_i64().unwrap() >= 8000);
        assert_eq!(body["messages"][0]["content"], "3 web servers");
    }

    #[test]
    fn the_openai_body_uses_strict_response_format() {
        let cfg = Config::new(Provider::OpenAi);
        let body: Value = serde_json::from_str(&request_body(
            &cfg,
            "sys",
            "3 web servers",
            &plan_schema(&services()),
        ))
        .unwrap();
        assert_eq!(
            body["response_format"]["json_schema"]["strict"],
            json!(true)
        );
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn a_gateway_base_url_joins_cleanly_with_or_without_a_trailing_slash() {
        assert_eq!(
            join_url("https://gw.internal/anthropic/", "/v1/messages"),
            "https://gw.internal/anthropic/v1/messages"
        );
        assert_eq!(
            join_url(" https://api.anthropic.com ", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn provider_names_are_forgiving_but_bounded() {
        assert_eq!(Provider::parse("claude").unwrap(), Provider::Anthropic);
        assert_eq!(Provider::parse("OpenAI").unwrap(), Provider::OpenAi);
        assert_eq!(Provider::parse("netdata").unwrap(), Provider::Netdata);
        assert_eq!(
            Provider::parse("llm.netdata.cloud").unwrap(),
            Provider::Netdata
        );
        assert!(Provider::parse("llama").is_err());
    }
}
