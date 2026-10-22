//! `gitagent validate`: checks an agent directory against the OpenGAP
//! format. Loading is lenient (problems become warnings and the agent still
//! runs); here the same problems are reported as errors where the format
//! requires them, with the file and, where it can be found, the line.
//!
//! Always checked: `agent.yaml` against its schema (closed objects, types,
//! ranges and patterns); the skills, tools and sub-agents it names exist;
//! `SOUL.md` has text; `hooks/hooks.yaml` and its scripts; every
//! `tools/*.yaml` and `skills/*/SKILL.md` (as warnings and errors as the
//! format grades them). With `compliance`, the regulatory rules for the
//! `compliance` section are checked as well.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match self.line {
            Some(l) => write!(f, "{level}: {}:{l}: {}", self.file, self.message),
            None => write!(f, "{level}: {}: {}", self.file, self.message),
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    fn count(&self, s: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == s).count()
    }
}

/// Collects findings for one file, finding lines for dotted paths.
struct Doc<'a> {
    file: &'a str,
    text: &'a str,
    out: &'a mut Vec<Finding>,
}

impl Doc<'_> {
    fn add(&mut self, severity: Severity, path: &str, message: String) {
        let line = line_of(self.text, path);
        let message = if path.is_empty() {
            message
        } else {
            format!("{path}: {message}")
        };
        self.out.push(Finding {
            severity,
            file: self.file.to_string(),
            line,
            message,
        });
    }

    fn error(&mut self, path: &str, message: impl Into<String>) {
        self.add(Severity::Error, path, message.into());
    }

    fn warn(&mut self, path: &str, message: impl Into<String>) {
        self.add(Severity::Warning, path, message.into());
    }
}

/// The line of the last key in a dotted path (`a.b.0.c`), found by looking
/// for each key in turn from the line of the one before (keys in flow
/// mappings like `{a: 1}` count); the deepest key found when one is not.
fn line_of(text: &str, path: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let is_key = |line: &str, key: &str| {
        let l = line.trim_start().trim_start_matches("- ");
        let starts = l
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        starts
            || [
                format!("{{{key}:"),
                format!("{{ {key}:"),
                format!(", {key}:"),
            ]
            .iter()
            .any(|p| l.contains(p.as_str()))
    };
    let mut at = 0;
    let mut found = None;
    for key in path
        .split('.')
        .filter(|k| !k.is_empty() && k.parse::<usize>().is_err())
    {
        let Some(hit) = (at..lines.len()).find(|&i| is_key(lines[i], key)) else {
            break;
        };
        at = hit;
        found = Some(hit + 1);
    }
    found
}

// ------------------------------------------------------------- schema

/// A value's expected shape.
#[derive(Clone, Copy)]
enum Ty {
    Str,
    Bool,
    /// An integer with a lower bound.
    Int(i64),
    /// A number within bounds.
    Num(f64, f64),
    Enum(&'static [&'static str]),
    /// A string matching a pattern, described for messages.
    Pat(fn(&str) -> bool, &'static str),
    List(&'static Ty),
    /// A closed object; keys with `!` are required.
    Obj(&'static [(&'static str, Ty)]),
    Any,
}

const STR_LIST: Ty = Ty::List(&Ty::Str);
const KEBAB_LIST: Ty = Ty::List(&Ty::Pat(is_kebab, "lowercase letters, digits and dashes"));
const CADENCE: &[&str] = &[
    "daily",
    "weekly",
    "monthly",
    "quarterly",
    "semi_annual",
    "annual",
];
const LEVELS: &[&str] = &["public", "internal", "confidential", "restricted"];

const MODEL: Ty = Ty::Obj(&[
    ("preferred", Ty::Str),
    ("fallback", STR_LIST),
    (
        "constraints",
        Ty::Obj(&[
            ("temperature", Ty::Num(0.0, 2.0)),
            ("max_tokens", Ty::Int(1)),
            ("top_p", Ty::Num(0.0, 1.0)),
            ("top_k", Ty::Int(1)),
            ("stop_sequences", STR_LIST),
            ("presence_penalty", Ty::Num(-2.0, 2.0)),
            ("frequency_penalty", Ty::Num(-2.0, 2.0)),
        ]),
    ),
]);

const DEPENDENCY: Ty = Ty::Obj(&[
    (
        "name!",
        Ty::Pat(is_kebab, "lowercase letters, digits and dashes"),
    ),
    ("source!", Ty::Str),
    ("version", Ty::Str),
    ("mount", Ty::Str),
    (
        "vendor_management",
        Ty::Obj(&[
            ("due_diligence_date", Ty::Str),
            ("soc_report", Ty::Bool),
            (
                "risk_assessment",
                Ty::Enum(&["low", "medium", "high", "critical"]),
            ),
        ]),
    ),
]);

const SUB_AGENT: Ty = Ty::Obj(&[
    ("description", Ty::Str),
    (
        "delegation",
        Ty::Obj(&[
            ("mode", Ty::Enum(&["auto", "explicit", "router"])),
            ("triggers", STR_LIST),
        ]),
    ),
]);

const COMPLIANCE: Ty = Ty::Obj(&[
    (
        "risk_tier",
        Ty::Enum(&["low", "standard", "high", "critical"]),
    ),
    ("frameworks", STR_LIST),
    (
        "supervision",
        Ty::Obj(&[
            ("designated_supervisor", Ty::Any),
            ("review_cadence", Ty::Enum(CADENCE)),
            (
                "human_in_the_loop",
                Ty::Enum(&["always", "conditional", "advisory", "none"]),
            ),
            ("escalation_triggers", Ty::List(&Ty::Any)),
            ("override_capability", Ty::Bool),
            ("kill_switch", Ty::Bool),
        ]),
    ),
    (
        "recordkeeping",
        Ty::Obj(&[
            ("audit_logging", Ty::Bool),
            ("log_format", Ty::Enum(&["structured_json", "plaintext"])),
            (
                "retention_period",
                Ty::Pat(is_period, "a period like 6y, 18m or 90d"),
            ),
            (
                "log_contents",
                Ty::List(&Ty::Enum(&[
                    "prompts_and_responses",
                    "tool_calls",
                    "decision_pathways",
                    "model_version",
                    "timestamps",
                ])),
            ),
            ("immutable", Ty::Bool),
        ]),
    ),
    (
        "model_risk",
        Ty::Obj(&[
            ("inventory_id", Ty::Any),
            (
                "validation_cadence",
                Ty::Enum(&["monthly", "quarterly", "semi_annual", "annual"]),
            ),
            (
                "validation_type",
                Ty::Enum(&["full", "targeted", "change_based"]),
            ),
            ("conceptual_soundness", Ty::Any),
            ("ongoing_monitoring", Ty::Bool),
            ("outcomes_analysis", Ty::Bool),
            ("drift_detection", Ty::Bool),
            ("parallel_testing", Ty::Bool),
        ]),
    ),
    (
        "data_governance",
        Ty::Obj(&[
            (
                "pii_handling",
                Ty::Enum(&["redact", "encrypt", "prohibit", "allow"]),
            ),
            ("data_classification", Ty::Enum(LEVELS)),
            ("consent_required", Ty::Bool),
            ("cross_border", Ty::Bool),
            ("bias_testing", Ty::Bool),
            ("lda_search", Ty::Bool),
        ]),
    ),
    (
        "communications",
        Ty::Obj(&[
            (
                "type",
                Ty::Enum(&["correspondence", "retail", "institutional"]),
            ),
            ("pre_review_required", Ty::Bool),
            ("fair_balanced", Ty::Bool),
            ("no_misleading", Ty::Bool),
            ("disclosures_required", Ty::Bool),
        ]),
    ),
    (
        "vendor_management",
        Ty::Obj(&[
            ("due_diligence_complete", Ty::Bool),
            ("soc_report_required", Ty::Bool),
            ("vendor_ai_notification", Ty::Bool),
            ("subcontractor_assessment", Ty::Bool),
        ]),
    ),
    (
        "segregation_of_duties",
        Ty::Obj(&[
            ("roles", Ty::List(&Ty::Any)),
            ("conflicts", Ty::List(&STR_LIST)),
            ("assignments", Ty::Any),
            (
                "isolation",
                Ty::Obj(&[
                    ("state", Ty::Enum(&["full", "shared", "none"])),
                    ("credentials", Ty::Enum(&["separate", "shared"])),
                ]),
            ),
            ("handoffs", Ty::List(&Ty::Any)),
            ("enforcement", Ty::Enum(&["strict", "advisory"])),
        ]),
    ),
    (
        "financial_governance",
        Ty::Obj(&[
            ("enabled!", Ty::Bool),
            ("firewall", Ty::Str),
            (
                "spending",
                Ty::Obj(&[
                    ("max_per_transaction_cents!", Ty::Any),
                    ("max_monthly_cents", Ty::Int(1)),
                    ("currency", Ty::Str),
                    ("allowed_categories", STR_LIST),
                    ("blocked_categories", STR_LIST),
                ]),
            ),
            (
                "approval",
                Ty::Obj(&[
                    ("require_above_cents!", Ty::Int(0)),
                    ("timeout_minutes!", Ty::Int(1)),
                    ("auto_deny_on_timeout!", Ty::Bool),
                ]),
            ),
        ]),
    ),
]);

const MANIFEST: Ty = Ty::Obj(&[
    (
        "spec_version",
        Ty::Pat(is_spec_version, "a version like 0.1.0"),
    ),
    (
        "name!",
        Ty::Pat(
            is_kebab,
            "lowercase letters, digits and dashes, starting with a letter",
        ),
    ),
    (
        "version!",
        Ty::Pat(is_semver, "a semantic version like 1.0.0"),
    ),
    (
        "description!",
        Ty::Pat(|s| !s.trim().is_empty(), "a non-empty text"),
    ),
    ("author", Ty::Str),
    ("license", Ty::Str),
    ("model", MODEL),
    ("extends", Ty::Str),
    ("dependencies", Ty::List(&DEPENDENCY)),
    ("skills", KEBAB_LIST),
    ("tools", KEBAB_LIST),
    ("agents", Ty::Any),
    (
        "delegation",
        Ty::Obj(&[
            ("mode", Ty::Enum(&["auto", "explicit", "router"])),
            ("router", Ty::Str),
        ]),
    ),
    (
        "runtime",
        Ty::Obj(&[
            ("max_turns", Ty::Int(1)),
            ("temperature", Ty::Num(0.0, 2.0)),
            ("timeout", Ty::Int(1)),
        ]),
    ),
    (
        "a2a",
        Ty::Obj(&[
            ("url", Ty::Str),
            ("capabilities", STR_LIST),
            (
                "authentication",
                Ty::Obj(&[
                    ("type", Ty::Enum(&["none", "api_key", "oauth2", "mtls"])),
                    ("credentials_ref", Ty::Str),
                ]),
            ),
            ("protocols", Ty::List(&Ty::Enum(&["a2a", "mcp"]))),
        ]),
    ),
    ("compliance", COMPLIANCE),
    (
        "registries",
        Ty::List(&Ty::Obj(&[("name!", Ty::Str), ("url", Ty::Str)])),
    ),
    ("tags", STR_LIST),
    ("mcp_servers", Ty::Any),
    ("metadata", Ty::Any),
    // Plugin settings of gitagent 1.x agents; reported separately.
    ("plugins", Ty::Any),
]);

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Null => "nothing",
        Value::Bool(_) => "true/false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}

fn check(doc: &mut Doc, path: &str, v: &Value, ty: Ty) {
    let at = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    match ty {
        Ty::Any => {}
        Ty::Str => {
            if !v.is_string() {
                doc.error(path, format!("must be text, not {}", describe(v)));
            }
        }
        Ty::Bool => {
            if !v.is_boolean() {
                doc.error(path, format!("must be true or false, not {}", describe(v)));
            }
        }
        Ty::Int(min) => match v.as_i64() {
            Some(n) if n >= min => {}
            Some(n) => doc.error(path, format!("must be at least {min} (got {n})")),
            None => doc.error(path, format!("must be a whole number, not {}", describe(v))),
        },
        Ty::Num(lo, hi) => match v.as_f64() {
            Some(n) if (lo..=hi).contains(&n) => {}
            Some(n) => doc.error(path, format!("must be between {lo} and {hi} (got {n})")),
            None => doc.error(path, format!("must be a number, not {}", describe(v))),
        },
        Ty::Enum(options) => match v.as_str() {
            Some(s) if options.contains(&s) => {}
            _ => doc.error(
                path,
                format!("must be one of {} (got {v})", options.join(", ")),
            ),
        },
        Ty::Pat(ok, what) => match v.as_str() {
            Some(s) if ok(s) => {}
            Some(s) => doc.error(path, format!("must be {what} (got {s:?})")),
            None => doc.error(path, format!("must be {what}, not {}", describe(v))),
        },
        Ty::List(item) => match v.as_array() {
            Some(items) => {
                for (i, x) in items.iter().enumerate() {
                    check(doc, &at(&i.to_string()), x, *item);
                }
            }
            None => doc.error(path, format!("must be a list, not {}", describe(v))),
        },
        Ty::Obj(fields) => {
            let Some(map) = v.as_object() else {
                doc.error(path, format!("must be a mapping, not {}", describe(v)));
                return;
            };
            for (key, ty) in fields {
                let (name, required) = match key.strip_suffix('!') {
                    Some(n) => (n, true),
                    None => (*key, false),
                };
                match map.get(name) {
                    Some(x) => check(doc, &at(name), x, *ty),
                    None if required => doc.error(path, format!("`{name}` is required")),
                    None => {}
                }
            }
            for key in map.keys() {
                if !fields.iter().any(|(k, _)| k.trim_end_matches('!') == key) {
                    doc.error(&at(key), "is not a known setting");
                }
            }
        }
    }
}

/// `^[a-z][a-z0-9-]*$`
fn is_kebab(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^\d+\.\d+\.\d+$`
fn is_spec_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// SemVer with optional `-pre` and `+build` parts.
fn is_semver(s: &str) -> bool {
    let (core, build) = s.split_once('+').map_or((s, None), |(c, b)| (c, Some(b)));
    let (core, pre) = core
        .split_once('-')
        .map_or((core, None), |(c, p)| (c, Some(p)));
    let extra_ok = |x: Option<&str>| {
        x.is_none_or(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'))
    };
    is_spec_version(core) && extra_ok(pre) && extra_ok(build)
}

/// `^\d+[ymd]$`
fn is_period(s: &str) -> bool {
    s.len() >= 2
        && s[..s.len() - 1].chars().all(|c| c.is_ascii_digit())
        && matches!(s.chars().last(), Some('y' | 'm' | 'd'))
}

/// A skill name: kebab-case without `--` or a dash at either end, at most 64.
fn is_skill_name(s: &str) -> bool {
    is_kebab(s) && !s.contains("--") && !s.ends_with('-') && s.len() <= 64
}

// ------------------------------------------------------------- checks

pub fn validate(root: &Path, compliance: bool) -> Report {
    let mut out = Vec::new();
    let path = root.join("agent.yaml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        out.push(Finding {
            severity: Severity::Error,
            file: "agent.yaml".into(),
            line: None,
            message: "missing (an agent directory needs agent.yaml and SOUL.md)".into(),
        });
        return Report { findings: out };
    };
    let manifest: Value = match serde_saphyr::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            out.push(Finding {
                severity: Severity::Error,
                file: "agent.yaml".into(),
                line: None,
                message: format!("not valid YAML: {e}"),
            });
            return Report { findings: out };
        }
    };
    {
        let mut doc = Doc {
            file: "agent.yaml",
            text: &text,
            out: &mut out,
        };
        check(&mut doc, "", &manifest, MANIFEST);
        manifest_extras(&mut doc, &manifest, root);
        if compliance {
            compliance_rules(&mut doc, &manifest, root);
        }
    }
    soul(root, &mut out);
    hooks(root, &mut out);
    tool_files(root, &mut out);
    skill_files(root, &mut out);
    config_files(root, &mut out);
    knowledge_index(root, &mut out);
    out.sort_by(|a, b| (a.severity, &a.file, a.line).cmp(&(b.severity, &b.file, b.line)));
    Report { findings: out }
}

/// What the closed schema cannot say: unique lists, references to files,
/// MCP servers, metadata values and 1.x plugins.
fn manifest_extras(doc: &mut Doc, m: &Value, root: &Path) {
    for key in ["skills", "tools", "tags"] {
        let items: Vec<&str> = m[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let unique: BTreeSet<&str> = items.iter().copied().collect();
        if unique.len() != items.len() {
            doc.error(key, "lists a name more than once");
        }
    }
    for name in m["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let dir = root.join("skills").join(name);
        if !dir.is_dir() {
            doc.error("skills", format!("skills/{name}/ does not exist"));
        } else if !dir.join("SKILL.md").is_file() {
            doc.warn("skills", format!("skills/{name}/ has no SKILL.md"));
        }
    }
    for name in m["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !root.join("tools").join(format!("{name}.yaml")).is_file() {
            doc.error("tools", format!("tools/{name}.yaml does not exist"));
        }
    }
    if let Some(src) = m["extends"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if crate::manifest::is_git_source(src) {
            if !root.join(".gitagent/parent/agent.yaml").is_file() {
                doc.warn(
                    "extends",
                    "the parent is not installed; run `gitagent install`",
                );
            }
        } else {
            let parent = if Path::new(src).is_absolute() {
                PathBuf::from(src)
            } else {
                root.join(src)
            };
            if !parent.join("agent.yaml").is_file() {
                doc.error("extends", format!("the parent {src} has no agent.yaml"));
            }
        }
    }
    for dep in m["dependencies"].as_array().into_iter().flatten() {
        if let Some((name, mount)) = crate::manifest::dependency_mount(root, dep) {
            if !mount.join("agent.yaml").is_file() {
                doc.warn(
                    "dependencies",
                    format!(
                        "{name:?} is not installed at {}; run `gitagent install`",
                        mount.display()
                    ),
                );
            }
        }
    }
    if let Some(agents) = m["agents"].as_object() {
        for (name, cfg) in agents {
            check(doc, &format!("agents.{name}"), cfg, SUB_AGENT);
            let dir = root.join("agents").join(name);
            if !dir.is_dir() && !root.join("agents").join(format!("{name}.md")).is_file() {
                doc.error(
                    &format!("agents.{name}"),
                    format!("neither agents/{name}/ nor agents/{name}.md exists"),
                );
            }
        }
    } else if !m["agents"].is_null() {
        doc.error("agents", "must be a mapping of sub-agent names");
    }
    if let Some(servers) = m["mcp_servers"].as_object() {
        for (name, cfg) in servers {
            if let Err(why) = crate::mcp::parse(name, cfg, root) {
                doc.error(&format!("mcp_servers.{name}"), why);
            }
        }
    } else if !m["mcp_servers"].is_null() {
        doc.error("mcp_servers", "must be a mapping of server names");
    }
    if let Some(meta) = m["metadata"].as_object() {
        for (k, v) in meta {
            if !(v.is_string() || v.is_number() || v.is_boolean()) {
                doc.error(
                    &format!("metadata.{k}"),
                    "must be text, a number or true/false",
                );
            }
        }
    } else if !m["metadata"].is_null() {
        doc.error("metadata", "must be a mapping");
    }
    if !m["plugins"].is_null() {
        doc.warn(
            "plugins",
            "is a gitagent 1.x setting, not part of the OpenGAP format",
        );
    }
    if let Some(mode) = m["delegation"]["mode"].as_str() {
        if mode == "router" && m["delegation"]["router"].as_str().is_none() {
            doc.error(
                "delegation",
                "mode `router` needs `router` (the router agent's name)",
            );
        }
    }
    for (i, t) in m["compliance"]["supervision"]["escalation_triggers"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let path = format!("compliance.supervision.escalation_triggers.{i}");
        let known = [
            "confidence_below",
            "action_type",
            "error_detected",
            "data_classification_above",
            "token_count_above",
            "custom",
        ];
        match t.as_object() {
            Some(o) if o.len() == 1 && known.contains(&o.keys().next().map_or("", |k| k)) => {}
            _ => doc.error(
                &path,
                format!("must be one of {{{}: …}}", known.join(": …} or {")),
            ),
        }
    }
}

fn soul(root: &Path, out: &mut Vec<Finding>) {
    let message = match std::fs::read_to_string(root.join("SOUL.md")) {
        Err(_) => "missing (it is required)",
        Ok(t) if t.trim().is_empty() => "is empty",
        Ok(t)
            if !t
                .lines()
                .any(|l| !l.trim().is_empty() && !l.trim().starts_with('#')) =>
        {
            "has headings but no text"
        }
        Ok(_) => return,
    };
    out.push(Finding {
        severity: Severity::Error,
        file: "SOUL.md".into(),
        line: None,
        message: message.into(),
    });
}

const HOOK_EVENTS: &[&str] = &[
    "on_session_start",
    "on_session_end",
    "pre_tool_use",
    "post_tool_use",
    "on_error",
    "pre_response",
    "post_response",
];

fn hooks(root: &Path, out: &mut Vec<Finding>) {
    let Ok(text) = std::fs::read_to_string(root.join("hooks/hooks.yaml")) else {
        return;
    };
    let mut doc = Doc {
        file: "hooks/hooks.yaml",
        text: &text,
        out,
    };
    let v: Value = match serde_saphyr::from_str(&text) {
        Ok(v) => v,
        Err(e) => return doc.error("", format!("not valid YAML: {e}")),
    };
    let Some(events) = v["hooks"].as_object() else {
        return doc.error("", "`hooks` (a mapping of events) is required");
    };
    for (event, list) in events {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            if matches!(
                event.as_str(),
                "pre_query" | "post_tool_failure" | "file_changed" | "user_prompt_submit"
            ) {
                doc.warn(
                    &format!("hooks.{event}"),
                    "is a gitagent extension, not one of the OpenGAP events",
                );
            } else {
                doc.error(&format!("hooks.{event}"), "is not a known event");
                continue;
            }
        }
        let Some(entries) = list.as_array() else {
            doc.error(&format!("hooks.{event}"), "must be a list of hooks");
            continue;
        };
        for (i, e) in entries.iter().enumerate() {
            let path = format!("hooks.{event}.{i}");
            let Some(map) = e.as_object() else {
                doc.error(&path, "must be a mapping");
                continue;
            };
            for key in map.keys() {
                match key.as_str() {
                    "script" | "description" | "timeout" | "compliance" | "fail_open" => {}
                    "applies_to" | "timeout_seconds" | "name" => doc.warn(
                        &format!("{path}.{key}"),
                        "is a gitagent extension, not in the OpenGAP hook schema",
                    ),
                    _ => doc.error(&format!("{path}.{key}"), "is not a known setting"),
                }
            }
            match map.get("script").and_then(Value::as_str) {
                Some(s) if root.join("hooks").join(s).is_file() => {}
                Some(s) => doc.error(
                    &format!("{path}.script"),
                    format!("hooks/{s} does not exist"),
                ),
                None => doc.error(&path, "`script` is required"),
            }
            if map.get("description").and_then(Value::as_str).is_none() {
                doc.error(&path, "`description` is required");
            }
            if let Some(t) = map.get("timeout") {
                if t.as_u64().is_none_or(|n| n < 1) {
                    doc.error(
                        &format!("{path}.timeout"),
                        "must be a whole number of seconds",
                    );
                }
            }
            for key in ["compliance", "fail_open"] {
                if map.get(key).is_some_and(|x| !x.is_boolean()) {
                    doc.error(&format!("{path}.{key}"), "must be true or false");
                }
            }
        }
    }
}

/// Every `tools/*.yaml`; problems are warnings, as the format grades them.
fn tool_files(root: &Path, out: &mut Vec<Finding>) {
    let mut files: Vec<_> = std::fs::read_dir(root.join("tools"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")))
        .collect();
    files.sort();
    for file in files {
        let name = format!(
            "tools/{}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let mut doc = Doc {
            file: &name,
            text: &text,
            out,
        };
        let v: Value = match serde_saphyr::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                doc.warn("", format!("not valid YAML: {e}"));
                continue;
            }
        };
        match v["name"].as_str() {
            Some(n) if is_kebab(n) => {}
            Some(n) => doc.warn(
                "name",
                format!("must be lowercase letters, digits and dashes (got {n:?})"),
            ),
            None => doc.warn("", "`name` is required"),
        }
        if v["description"]
            .as_str()
            .is_none_or(|d| d.trim().is_empty())
        {
            doc.warn("", "`description` is required");
        }
        match v["input_schema"].as_object() {
            Some(s) if s.get("type").and_then(Value::as_str) == Some("object") => {}
            Some(_) => doc.warn("input_schema", "must have `type: object`"),
            None => doc.warn("", "`input_schema` is required"),
        }
        let imp = &v["implementation"];
        if let Some(kind) = imp["type"].as_str() {
            if !matches!(kind, "script" | "mcp_server" | "http") {
                doc.warn("implementation.type", "must be script, mcp_server or http");
            }
            if kind == "script" {
                match imp["path"].as_str() {
                    Some(p) if root.join("tools").join(p).is_file() => {}
                    Some(p) => doc.warn("implementation.path", format!("tools/{p} does not exist")),
                    None => doc.warn("implementation", "a script tool needs `path`"),
                }
                if let Some(r) = imp["runtime"].as_str() {
                    if !matches!(r, "node" | "python3" | "bash" | "deno" | "ruby" | "go") {
                        doc.warn(
                            "implementation.runtime",
                            format!("{r:?} is not a known runtime"),
                        );
                    }
                }
            }
            if matches!(kind, "http" | "mcp_server") && imp["url"].as_str().is_none() {
                doc.warn("implementation", format!("a {kind} tool needs `url`"));
            }
        } else if imp.get("script").is_some() {
            doc.warn(
                "implementation",
                "the 1.x layout (`script:`); OpenGAP uses `type: script` and `path:`",
            );
        }
    }
}

/// `knowledge/index.yaml`; problems are warnings (the format leaves it
/// unchecked).
fn knowledge_index(root: &Path, out: &mut Vec<Finding>) {
    let Ok(text) = std::fs::read_to_string(root.join("knowledge/index.yaml")) else {
        return;
    };
    let mut doc = Doc {
        file: "knowledge/index.yaml",
        text: &text,
        out,
    };
    let v: Value = match serde_saphyr::from_str(&text) {
        Ok(v) => v,
        Err(e) => return doc.warn("", format!("not valid YAML: {e}")),
    };
    let list = match (v.get("documents"), v.get("entries")) {
        (Some(Value::Array(l)), _) => l,
        (None, Some(Value::Array(l))) => {
            doc.warn(
                "entries",
                "is the gitagent 1.x key; OpenGAP calls the list `documents`",
            );
            l
        }
        _ => return doc.warn("", "`documents` (a list) is required"),
    };
    for (i, item) in list.iter().enumerate() {
        let path = format!("documents.{i}");
        let Some(map) = item.as_object() else {
            doc.warn(&path, "must be a mapping");
            continue;
        };
        for key in map.keys() {
            if !matches!(
                key.as_str(),
                "path" | "tags" | "priority" | "always_load" | "description"
            ) {
                doc.warn(&format!("{path}.{key}"), "is not a known setting");
            }
        }
        match map.get("path").and_then(Value::as_str) {
            Some(p) if root.join("knowledge").join(p).is_file() => {}
            Some(p) => doc.warn(
                &format!("{path}.path"),
                format!("knowledge/{p} does not exist"),
            ),
            None => doc.warn(&path, "`path` is required"),
        }
        if let Some(tags) = map.get("tags") {
            let list: Vec<&str> = tags
                .as_array()
                .map(|t| t.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let unique: BTreeSet<&str> = list.iter().copied().collect();
            if !tags.is_array() || tags.as_array().is_some_and(|t| t.len() != list.len()) {
                doc.warn(&format!("{path}.tags"), "must be a list of words");
            } else if unique.len() != list.len() {
                doc.warn(&format!("{path}.tags"), "lists a tag more than once");
            }
        }
        if let Some(p) = map.get("priority") {
            if !matches!(p.as_str(), Some("low" | "medium" | "high")) {
                doc.warn(&format!("{path}.priority"), "must be low, medium or high");
            }
        }
        if map.get("always_load").is_some_and(|a| !a.is_boolean()) {
            doc.warn(&format!("{path}.always_load"), "must be true or false");
        }
    }
}

/// Every `config/*.yaml`; problems are warnings (the format leaves these
/// files unchecked, and agents keep their own settings in them).
fn config_files(root: &Path, out: &mut Vec<Finding>) {
    let mut files: Vec<_> = std::fs::read_dir(root.join("config"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
        .collect();
    files.sort();
    for file in files {
        let name = format!(
            "config/{}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let mut doc = Doc {
            file: &name,
            text: &text,
            out,
        };
        let v: Value = match serde_saphyr::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                doc.warn("", format!("not valid YAML: {e}"));
                continue;
            }
        };
        let Some(map) = v.as_object() else {
            if !v.is_null() {
                doc.warn("", "must be a mapping of settings");
            }
            continue;
        };
        let known = Ty::Obj(&[
            ("log_level", Ty::Enum(&["debug", "info", "warn", "error"])),
            ("compliance_mode", Ty::Bool),
            ("audit_logging", Ty::Bool),
            ("model_override", Ty::Any),
            ("temperature_override", Ty::Any),
            ("timeout_override", Ty::Any),
            ("env_vars", Ty::Any),
        ]);
        let Ty::Obj(fields) = known else { continue };
        for (key, ty) in fields {
            let Some(x) = map.get(*key).filter(|x| !x.is_null()) else {
                continue;
            };
            let mut problems = Vec::new();
            let mut inner = Doc {
                file: &name,
                text: &text,
                out: &mut problems,
            };
            check(&mut inner, key, x, *ty);
            for p in problems {
                doc.out.push(Finding {
                    severity: Severity::Warning,
                    ..p
                });
            }
        }
        match map.get("model_override") {
            None | Some(Value::Null) | Some(Value::String(_)) => {}
            Some(_) => doc.warn("model_override", "must be a model name"),
        }
        match map.get("temperature_override") {
            None | Some(Value::Null) => {}
            Some(t) if t.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)) => {}
            Some(_) => doc.warn("temperature_override", "must be a number between 0 and 2"),
        }
        match map.get("timeout_override") {
            None | Some(Value::Null) => {}
            Some(t) if t.as_u64().is_some_and(|t| t >= 1) => {}
            Some(_) => doc.warn("timeout_override", "must be a whole number of seconds"),
        }
        match map.get("env_vars") {
            None | Some(Value::Null) | Some(Value::Object(_)) => {}
            Some(_) => doc.warn("env_vars", "must be a mapping of variable names to values"),
        }
    }
}

/// Every `skills/*/SKILL.md`.
fn skill_files(root: &Path, out: &mut Vec<Finding>) {
    let mut dirs: Vec<_> = std::fs::read_dir(root.join("skills"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("SKILL.md").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let file = format!("skills/{dir_name}/SKILL.md");
        let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
        let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        let mut doc = Doc {
            file: &file,
            text: &text,
            out,
        };
        let (front, body) = crate::split_frontmatter(&text);
        if front.trim().is_empty() {
            doc.error(
                "",
                "has no YAML frontmatter (--- name: … description: … ---)",
            );
            continue;
        }
        let fm: Map<String, Value> = match serde_saphyr::from_str::<Value>(front) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                doc.error("", "the frontmatter must be a mapping");
                continue;
            }
            Err(e) => {
                doc.error("", format!("the frontmatter is not valid YAML: {e}"));
                continue;
            }
        };
        match fm.get("name").and_then(Value::as_str) {
            Some(n) if is_skill_name(n) => {
                if n != dir_name {
                    doc.warn("name", format!("{n:?} differs from the directory name {dir_name:?}"));
                }
            }
            Some(n) => doc.error(
                "name",
                format!("must be lowercase words joined by single dashes, at most 64 characters (got {n:?})"),
            ),
            None => doc.error("", "`name` is required"),
        }
        match fm.get("description").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() && d.chars().count() <= 1024 => {}
            Some(_) => doc.error("description", "must be 1 to 1024 characters"),
            None => doc.error("", "`description` is required"),
        }
        if fm
            .get("compatibility")
            .and_then(Value::as_str)
            .is_some_and(|c| c.chars().count() > 500)
        {
            doc.error("compatibility", "must be at most 500 characters");
        }
        for key in fm.keys() {
            if !matches!(
                key.as_str(),
                "name" | "description" | "license" | "compatibility" | "allowed-tools" | "metadata"
            ) {
                doc.error(
                    key,
                    "is not a known setting (extensions go under `metadata`)",
                );
            }
        }
        if body.chars().count() > 20_000 {
            doc.warn("", "the instructions are over 20000 characters; keep a skill short and put detail in files next to it");
        }
    }
}

// --------------------------------------------------------- compliance

fn compliance_rules(doc: &mut Doc, m: &Value, root: &Path) {
    let c = &m["compliance"];
    if !c.is_object() {
        doc.warn("", "there is no `compliance` section");
        return;
    }
    let tier = c["risk_tier"].as_str();
    if tier.is_none() {
        doc.error("compliance", "`risk_tier` is required");
    }
    let high = matches!(tier, Some("high" | "critical"));
    let frameworks: Vec<&str> = c["frameworks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let has = |f: &str| frameworks.contains(&f);
    if high {
        if !matches!(
            c["supervision"]["human_in_the_loop"].as_str(),
            Some("always" | "conditional")
        ) {
            doc.error(
                "compliance.risk_tier",
                "[FINRA 3110] a high or critical risk tier needs supervision.human_in_the_loop always or conditional",
            );
        }
        if c["recordkeeping"]["audit_logging"] != Value::Bool(true) {
            doc.error(
                "compliance.risk_tier",
                "[FINRA 4511] a high or critical risk tier needs recordkeeping.audit_logging: true",
            );
        }
        if matches!(
            c["model_risk"]["validation_cadence"].as_str(),
            Some("annual" | "semi_annual")
        ) {
            doc.error(
                "compliance.model_risk.validation_cadence",
                "[SR 11-7] a high or critical risk tier must validate quarterly or more often",
            );
        }
        for f in [
            "compliance/",
            "compliance/risk-assessment.md",
            "compliance/regulatory-map.yaml",
            "compliance/validation-schedule.yaml",
        ] {
            if !root.join(f).exists() {
                doc.warn("compliance.risk_tier", format!("{f} is missing"));
            }
        }
    }
    if has("finra") {
        if c["communications"]["fair_balanced"] != Value::Bool(true) {
            doc.error(
                "compliance.frameworks",
                "[FINRA 2210] finra needs communications.fair_balanced: true",
            );
        }
        if c["communications"]["no_misleading"] != Value::Bool(true) {
            doc.error(
                "compliance.frameworks",
                "[FINRA 2210] finra needs communications.no_misleading: true",
            );
        }
        if c["supervision"].is_null() {
            doc.warn(
                "compliance.frameworks",
                "finra without a `supervision` section",
            );
        }
        if c["recordkeeping"].is_null() {
            doc.warn(
                "compliance.frameworks",
                "finra without a `recordkeeping` section",
            );
        }
    }
    if has("federal_reserve") {
        if c["model_risk"].is_null() {
            doc.error(
                "compliance.frameworks",
                "[SR 11-7] federal_reserve needs a `model_risk` section",
            );
        } else if c["model_risk"]["ongoing_monitoring"] != Value::Bool(true) {
            doc.error(
                "compliance.frameworks",
                "[SR 11-7] federal_reserve needs model_risk.ongoing_monitoring: true",
            );
        }
    }
    if has("sec") {
        if c["recordkeeping"]["audit_logging"] != Value::Bool(true) {
            doc.warn(
                "compliance.frameworks",
                "sec without recordkeeping.audit_logging: true",
            );
        }
        if c["data_governance"]["pii_handling"] == "allow" {
            doc.warn(
                "compliance.frameworks",
                "sec with data_governance.pii_handling: allow",
            );
        }
    }
    if has("cfpb") && c["data_governance"]["bias_testing"] != Value::Bool(true) {
        doc.warn(
            "compliance.frameworks",
            "cfpb without data_governance.bias_testing: true",
        );
    }
    if (has("finra") || has("federal_reserve"))
        && m["dependencies"]
            .as_array()
            .is_some_and(|d| d.iter().any(|x| x["vendor_management"].is_null()))
    {
        doc.warn(
            "dependencies",
            "[SR 23-4] a dependency has no vendor_management section",
        );
    }
    sod_rules(doc, m, high);
    let fg = &c["financial_governance"];
    if fg["enabled"] == Value::Bool(true)
        && fg["spending"]["max_per_transaction_cents"]
            .as_i64()
            .is_some_and(|n| n <= 0)
    {
        doc.error(
            "compliance.financial_governance.spending.max_per_transaction_cents",
            "must be at least 1",
        );
    }
    if fg["firewall"]
        .as_str()
        .is_some_and(|f| f.starts_with("http"))
    {
        doc.error(
            "compliance.financial_governance.firewall",
            "must name the firewall, not give its endpoint URL",
        );
    }
    if fg.is_null() && high {
        let paying = [
            "payment",
            "purchase",
            "financial",
            "billing",
            "invoice",
            "stripe",
            "pay",
        ];
        if m["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|t| paying.iter().any(|p| t.contains(p)))
        {
            doc.warn(
                "tools",
                "a payment-like tool at a high or critical risk tier without financial_governance",
            );
        }
    }
}

fn sod_rules(doc: &mut Doc, m: &Value, high: bool) {
    let sod = &m["compliance"]["segregation_of_duties"];
    let base = "compliance.segregation_of_duties";
    if !sod.is_object() {
        if high && m["agents"].as_object().is_some_and(|a| a.len() >= 2) {
            doc.warn(
                "compliance",
                "two or more sub-agents at a high or critical risk tier without segregation_of_duties",
            );
        }
        return;
    }
    let roles: Vec<&str> = sod["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["id"].as_str())
        .collect();
    if roles.len() < 2 {
        doc.error(base, "`roles` needs at least two roles");
    }
    let unique: BTreeSet<&str> = roles.iter().copied().collect();
    if unique.len() != roles.len() {
        doc.error(&format!("{base}.roles"), "a role id is used more than once");
    }
    let known = |r: &str| unique.contains(r);
    let pairs: Vec<(String, String)> = sod["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
        .collect();
    if pairs.is_empty() {
        doc.warn(base, "no `conflicts` are defined");
    }
    for (a, b) in &pairs {
        for r in [a, b] {
            if !known(r) {
                doc.error(
                    &format!("{base}.conflicts"),
                    format!("{r:?} is not a defined role"),
                );
            }
        }
        if a == b {
            doc.error(
                &format!("{base}.conflicts"),
                format!("[{a}, {a}] pairs a role with itself"),
            );
        }
    }
    let strict = sod["enforcement"].as_str() != Some("advisory");
    let sub_agents = m["agents"].as_object();
    let mut assigned: BTreeSet<&str> = BTreeSet::new();
    for (agent, held) in sod["assignments"].as_object().into_iter().flatten() {
        let held: Vec<&str> = held
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for r in &held {
            assigned.insert(r);
            if !known(r) {
                doc.error(
                    &format!("{base}.assignments"),
                    format!("{agent:?} has {r:?}, which is not a defined role"),
                );
            }
        }
        for (a, b) in &pairs {
            if held.contains(&a.as_str()) && held.contains(&b.as_str()) {
                let message = format!("{agent:?} holds both {a} and {b}, which conflict");
                if strict {
                    doc.error(&format!("{base}.assignments"), message);
                } else {
                    doc.warn(&format!("{base}.assignments"), message);
                }
            }
        }
        let is_root = m["name"].as_str() == Some(agent.as_str());
        if sub_agents.is_some_and(|s| !s.contains_key(agent)) && !is_root {
            doc.warn(
                &format!("{base}.assignments"),
                format!("{agent:?} is not one of the agents"),
            );
        }
    }
    for r in &roles {
        if !assigned.contains(r) {
            doc.warn(base, format!("the role {r:?} is not assigned to any agent"));
        }
    }
    for h in sod["handoffs"].as_array().into_iter().flatten() {
        let needed: Vec<&str> = h["required_roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for r in &needed {
            if !known(r) {
                doc.error(
                    &format!("{base}.handoffs"),
                    format!("{r:?} is not a defined role"),
                );
            }
        }
        if needed.iter().collect::<BTreeSet<_>>().len() < 2 {
            doc.error(
                &format!("{base}.handoffs"),
                format!(
                    "the handoff {:?} needs at least two different roles",
                    h["action"].as_str().unwrap_or("?")
                ),
            );
        }
    }
    if high {
        if !strict {
            doc.warn(
                base,
                "enforcement is advisory at a high or critical risk tier",
            );
        }
        if sod["isolation"]["state"] != "full" {
            doc.warn(
                base,
                "isolation.state is not full at a high or critical risk tier",
            );
        }
        if sod["isolation"]["credentials"] != "separate" {
            doc.warn(
                base,
                "isolation.credentials is not separate at a high or critical risk tier",
            );
        }
    }
}

#[cfg(test)]
mod tests;
