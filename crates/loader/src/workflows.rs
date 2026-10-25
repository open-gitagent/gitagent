//! Workflows in an agent's `workflows/` (or `skillflows/`): fixed
//! procedures of steps that run in dependency order, each step a skill, a
//! tool or a sub-agent with an `action` as its instruction.
//!
//! ```yaml
//! name: regulatory-review
//! description: Review a document against the applicable rules.
//! inputs: [{name: document, type: file, required: true}]
//! steps:
//!   - id: classify
//!     action: Classify the document and name the applicable rules.
//!     skill: regulatory-analysis
//!     outputs: [doc_type, rules]
//!   - id: report
//!     action: Write the final report.
//!     tool: generate-report
//!     depends_on: [classify]
//!     inputs: {rules: "${{ steps.classify.outputs.rules }}"}
//!     conditions: ["${{ steps.classify.outputs.doc_type != 'other' }}"]
//!     compliance: {requires_approval: true}
//! error_handling: {on_step_failure: abort}
//! ```
//!
//! gitagent 1.x's shorter form (`steps: [{skill, prompt}]`, run in order)
//! and a `steps:` mapping keyed by id are read as well. `workflows/*.md`
//! are narrative procedures: listed, not run.
//!
//! Values and conditions use `${{ … }}` with paths `inputs.<name>`,
//! `steps.<id>.outputs.<name>` and `trigger.<name>`, and the comparisons
//! `==`, `!=`, `<`, `<=`, `>`, `>=` against numbers, quoted strings, `true`,
//! `false` and `null`. A bare path holds when its value is not null, false,
//! zero or empty.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    /// `workflows/<file>`, as shown to people.
    pub file: String,
    pub inputs: Vec<Input>,
    /// Named outputs of the whole workflow.
    pub outputs: Vec<String>,
    /// Empty for a narrative (Markdown) workflow.
    pub steps: Vec<Step>,
    pub on_step_failure: OnFailure,
    pub escalation_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub name: String,
    pub kind: String,
    pub description: Option<String>,
    pub required: bool,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub action: String,
    pub skill: Option<String>,
    pub tool: Option<String>,
    pub agent: Option<String>,
    pub inputs: Map<String, Value>,
    pub outputs: Vec<String>,
    pub depends_on: Vec<String>,
    pub conditions: Vec<String>,
    pub max_attempts: u32,
    pub backoff: Backoff,
    pub requires_approval: bool,
    pub audit_level: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backoff {
    Linear,
    Exponential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnFailure {
    #[default]
    Abort,
    Skip,
    Retry,
    Escalate,
}

impl Workflow {
    pub fn is_narrative(&self) -> bool {
        self.steps.is_empty()
    }

    /// The steps in an order that respects `depends_on` (file order where
    /// it allows), or the ids in a cycle.
    pub fn ordered_steps(&self) -> Result<Vec<&Step>, Vec<String>> {
        let mut done: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        let mut left: Vec<&Step> = self.steps.iter().collect();
        while !left.is_empty() {
            let i = left
                .iter()
                .position(|s| s.depends_on.iter().all(|d| done.contains(&d.as_str())));
            let Some(i) = i else {
                return Err(left.iter().map(|s| s.id.clone()).collect());
            };
            let step = left.remove(i);
            done.push(&step.id);
            out.push(step);
        }
        Ok(out)
    }
}

/// The workflows of `root`'s `workflows/` (or `skillflows/`), sorted by
/// name, and what was wrong.
pub fn discover(root: &Path) -> (Vec<Workflow>, Vec<String>) {
    let mut warnings = Vec::new();
    let dir = ["workflows", "skillflows"]
        .iter()
        .map(|d| root.join(d))
        .find(|d| d.is_dir());
    let Some(dir) = dir else {
        return (Vec::new(), warnings);
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let mut workflows: Vec<Workflow> = Vec::new();
    for file in files {
        let shown = format!(
            "{}/{}",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        let parsed = match file.extension().and_then(|e| e.to_str()) {
            Some("yaml" | "yml") => read_yaml(&file, &shown),
            Some("md") => read_markdown(&file, &shown),
            _ => continue,
        };
        match parsed {
            Ok(w) if workflows.iter().any(|o| o.name == w.name) => warnings.push(format!(
                "{shown}: a workflow named {:?} already exists",
                w.name
            )),
            Ok(w) => workflows.push(w),
            Err(why) => warnings.push(format!("{shown}: {why}")),
        }
    }
    workflows.sort_by(|a, b| a.name.cmp(&b.name));
    (workflows, warnings)
}

fn read_markdown(file: &Path, shown: &str) -> Result<Workflow, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let (front, _) = crate::split_frontmatter(&text);
    let fm: Value = if front.trim().is_empty() {
        Value::Null
    } else {
        serde_saphyr::from_str(front)
            .map_err(|e| format!("the frontmatter is not valid YAML: {e}"))?
    };
    let name = fm["name"].as_str().map(str::to_string).unwrap_or_else(|| {
        file.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    Ok(Workflow {
        name,
        description: fm["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
        file: shown.to_string(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        steps: Vec::new(),
        on_step_failure: OnFailure::Abort,
        escalation_target: None,
    })
}

fn read_yaml(file: &Path, shown: &str) -> Result<Workflow, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let mut v: Value = serde_saphyr::from_str(&text).map_err(|e| format!("not valid YAML: {e}"))?;
    steps_in_file_order(&text, &mut v);
    parse(&v, shown)
}

/// The keys of the `steps:` mapping in the order the file has them.
struct StepKeys(Vec<String>);

impl<'de> serde::Deserialize<'de> for StepKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Keys;
        impl<'de> serde::de::Visitor<'de> for Keys {
            type Value = StepKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("the steps")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<StepKeys, A::Error> {
                let mut keys = Vec::new();
                while let Some(k) = map.next_key::<String>()? {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    keys.push(k);
                }
                Ok(StepKeys(keys))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<StepKeys, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(StepKeys(Vec::new()))
            }
        }
        d.deserialize_any(Keys)
    }
}

/// The parsed value keeps mapping keys sorted, so a `steps:` mapping (the
/// README's form, keyed by step id) is turned into the list form in the
/// order of the file; steps without a prompt then still follow each other
/// as written.
fn steps_in_file_order(text: &str, v: &mut Value) {
    #[derive(serde::Deserialize)]
    struct Doc {
        #[serde(default)]
        steps: Option<StepKeys>,
    }
    let Some(Value::Object(map)) = v.get("steps") else {
        return;
    };
    let Ok(Doc {
        steps: Some(StepKeys(order)),
    }) = serde_saphyr::from_str::<Doc>(text)
    else {
        return;
    };
    if order.len() != map.len() {
        return;
    }
    let list: Vec<Value> = order
        .iter()
        .filter_map(|id| {
            let mut step = map.get(id)?.clone();
            if let Value::Object(o) = &mut step {
                o.insert("id".into(), Value::String(id.clone()));
            }
            Some(step)
        })
        .collect();
    if list.len() == order.len() {
        v["steps"] = Value::Array(list);
    }
}

/// A workflow from its YAML value.
pub fn parse(v: &Value, shown: &str) -> Result<Workflow, String> {
    let name = v["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or("no `name`")?;
    let strings = |x: &Value| -> Vec<String> {
        match x {
            Value::Array(a) => a
                .iter()
                .map(|s| s.as_str().map_or_else(|| s.to_string(), str::to_string))
                .collect(),
            Value::String(s) => vec![s.clone()],
            _ => Vec::new(),
        }
    };
    let mut inputs = Vec::new();
    for i in v["inputs"].as_array().into_iter().flatten() {
        let Some(n) = i["name"].as_str() else {
            return Err("an input has no `name`".into());
        };
        inputs.push(Input {
            name: n.to_string(),
            kind: i["type"].as_str().unwrap_or("string").to_string(),
            description: i["description"].as_str().map(str::to_string),
            required: i["required"].as_bool().unwrap_or(true),
            default: i.get("default").filter(|d| !d.is_null()).cloned(),
        });
    }
    let outputs: Vec<String> = v["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["name"].as_str().or(o.as_str()))
        .map(str::to_string)
        .collect();
    let mut steps = Vec::new();
    let raw_steps: Vec<(String, Value)> = match &v["steps"] {
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let id = s["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("step-{}", i + 1));
                (id, s.clone())
            })
            .collect(),
        // The README's form: a mapping keyed by step id.
        Value::Object(m) => m.iter().map(|(k, s)| (k.clone(), s.clone())).collect(),
        Value::Null => return Err("no `steps`".into()),
        _ => return Err("`steps` must be a list".into()),
    };
    if raw_steps.is_empty() {
        return Err("`steps` is empty".into());
    }
    let mut previous: Option<String> = None;
    for (id, s) in raw_steps {
        if steps.iter().any(|x: &Step| x.id == id) {
            return Err(format!("the step id {id:?} is used more than once"));
        }
        let action = s["action"]
            .as_str()
            .or(s["prompt"].as_str())
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let explicit_order = s.get("depends_on").is_some();
        let mut depends_on = strings(&s["depends_on"]);
        // 1.x flows run their steps in order.
        if !explicit_order && s.get("prompt").is_some() {
            depends_on.extend(previous.clone());
        }
        let retry = &s["retry"];
        steps.push(Step {
            action,
            skill: s["skill"]
                .as_str()
                .map(str::to_string)
                .filter(|x| !x.is_empty()),
            tool: s["tool"].as_str().map(str::to_string),
            agent: s["agent"].as_str().map(str::to_string),
            inputs: s["inputs"].as_object().cloned().unwrap_or_default(),
            outputs: strings(&s["outputs"]),
            depends_on,
            conditions: strings(&s["conditions"]),
            max_attempts: retry["max_attempts"]
                .as_u64()
                .map_or(1, |n| n.max(1) as u32),
            backoff: if retry["backoff"] == "exponential" {
                Backoff::Exponential
            } else {
                Backoff::Linear
            },
            requires_approval: s["compliance"]["requires_approval"] == Value::Bool(true),
            audit_level: s["compliance"]["audit_level"].as_str().map(str::to_string),
            id: id.clone(),
        });
        previous = Some(id);
    }
    for s in &steps {
        if s.action.is_empty() && s.skill.is_none() && s.tool.is_none() {
            return Err(format!("the step {:?} has no `action`", s.id));
        }
        for d in &s.depends_on {
            if !steps.iter().any(|x| &x.id == d) {
                return Err(format!(
                    "the step {:?} depends on an unknown step {d:?}",
                    s.id
                ));
            }
        }
    }
    let eh = &v["error_handling"];
    let on_step_failure = match eh["on_step_failure"].as_str().or(eh["on_failure"].as_str()) {
        None | Some("abort") => OnFailure::Abort,
        Some("skip") => OnFailure::Skip,
        Some("retry") => OnFailure::Retry,
        Some("escalate") => OnFailure::Escalate,
        Some(other) => return Err(format!("`error_handling.on_step_failure` must be abort, skip, retry or escalate, not {other:?}")),
    };
    let w = Workflow {
        name: name.to_string(),
        description: v["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
        file: shown.to_string(),
        inputs,
        outputs,
        steps,
        on_step_failure,
        escalation_target: eh["escalation_target"].as_str().map(str::to_string),
    };
    if let Err(cycle) = w.ordered_steps() {
        return Err(format!(
            "the steps {} depend on each other in a cycle",
            cycle.join(", ")
        ));
    }
    Ok(w)
}

/// The `# Workflows` section of the system prompt, or nothing.
pub fn prompt_section(workflows: &[Workflow]) -> String {
    if workflows.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "# Workflows\n\nFixed procedures of the agent. Run one with the `workflow` tool when the person asks for it or its purpose fits; its steps run in their set order, not yours.\n",
    );
    for w in workflows {
        let mut line = format!("- {}: {}", w.name, w.description);
        if w.is_narrative() {
            line.push_str(&format!(" (a written procedure to read in {})", w.file));
        } else if !w.inputs.is_empty() {
            let names: Vec<String> = w
                .inputs
                .iter()
                .map(|i| {
                    if i.required {
                        i.name.clone()
                    } else {
                        format!("{}?", i.name)
                    }
                })
                .collect();
            line.push_str(&format!(" Inputs: {}.", names.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

// ------------------------------------------------------------ expressions

/// `value` with every `${{ … }}` filled in from `ctx`; a string that is
/// one expression becomes that expression's value, keeping its type.
pub fn substitute(value: &Value, ctx: &Value) -> Value {
    match value {
        Value::String(s) => {
            let t = s.trim();
            if let Some(inner) = t.strip_prefix("${{").and_then(|r| r.strip_suffix("}}")) {
                if !inner.contains("${{") {
                    return resolve(inner, ctx);
                }
            }
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(start) = rest.find("${{") {
                let Some(len) = rest[start..].find("}}") else {
                    break;
                };
                out.push_str(&rest[..start]);
                let v = resolve(&rest[start + 3..start + len], ctx);
                out.push_str(&match v {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    other => other.to_string(),
                });
                rest = &rest[start + len + 2..];
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, ctx)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), substitute(x, ctx)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// One expression: a literal, a path into `ctx`, or a comparison (which
/// gives true or false).
pub fn resolve(expr: &str, ctx: &Value) -> Value {
    let expr = expr.trim();
    if let Some((lhs, op, rhs)) = split_comparison(expr) {
        return Value::Bool(compare(&resolve(lhs, ctx), op, &resolve(rhs, ctx)));
    }
    if let Some(s) = quoted(expr) {
        return Value::String(s);
    }
    match expr {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" | "" => return Value::Null,
        _ => {}
    }
    if let Ok(n) = expr.parse::<f64>() {
        return serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number);
    }
    let mut cur = ctx;
    for key in expr.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(key).unwrap_or(&Value::Null),
            Value::Array(a) => key
                .parse::<usize>()
                .ok()
                .and_then(|i| a.get(i))
                .unwrap_or(&Value::Null),
            _ => &Value::Null,
        };
    }
    cur.clone()
}

/// Whether a condition holds: a comparison's result, or a value that is
/// not null, false, zero or empty.
pub fn holds(expr: &str, ctx: &Value) -> bool {
    let inner = expr
        .trim()
        .strip_prefix("${{")
        .and_then(|r| r.strip_suffix("}}"))
        .unwrap_or(expr);
    truthy(&resolve(inner, ctx))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn quoted(s: &str) -> Option<String> {
    for q in ['\'', '"'] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return Some(s[1..s.len() - 1].to_string());
        }
    }
    None
}

/// `lhs`, the operator and `rhs` of a comparison, outside quotes.
fn split_comparison(expr: &str) -> Option<(&str, &str, &str)> {
    let bytes = expr.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'\'' || c == b'"' => quote = Some(c),
            None => {
                for op in ["==", "!=", ">=", "<=", ">", "<"] {
                    if expr[i..].starts_with(op) {
                        return Some((&expr[..i], op, &expr[i + op.len()..]));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn compare(a: &Value, op: &str, b: &Value) -> bool {
    let nums = (a.as_f64(), b.as_f64());
    match op {
        "==" => a == b || matches!(nums, (Some(x), Some(y)) if x == y),
        "!=" => !(a == b || matches!(nums, (Some(x), Some(y)) if x == y)),
        _ => match nums {
            (Some(x), Some(y)) => match op {
                ">" => x > y,
                ">=" => x >= y,
                "<" => x < y,
                "<=" => x <= y,
                _ => false,
            },
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steps_mapping_keeps_the_order_of_the_file() {
        let text = "name: w\nsteps:\n  gather: {prompt: Collect the data}\n  analyze: {prompt: Analyze it}\n";
        let mut v: Value = serde_saphyr::from_str(text).unwrap();
        steps_in_file_order(text, &mut v);
        let wf = parse(&v, "w.yaml").unwrap();
        let ids: Vec<&str> = wf.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["gather", "analyze"]);
        assert_eq!(wf.steps[1].depends_on, ["gather"]);
        assert!(wf.steps[0].depends_on.is_empty());
    }
    use serde_json::json;

    #[test]
    fn both_forms_parse_and_steps_are_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        std::fs::write(
            root.join("workflows/review.yaml"),
            "name: review\ndescription: Review a document.\ninputs:\n  - {name: document, type: file}\n  - {name: framework, default: all, required: false}\noutputs: [{name: report, type: string}]\nsteps:\n  - id: report\n    action: Write the report.\n    tool: generate-report\n    depends_on: [verify, review]\n    outputs: [report]\n  - id: classify\n    action: Classify the document.\n    skill: regulatory-analysis\n    outputs: [doc_type, rules]\n    compliance: {audit_level: detailed}\n  - id: verify\n    action: Verify the claims.\n    agent: fact-checker\n    depends_on: [classify]\n    inputs: {claims: \"${{ steps.classify.outputs.rules }}\"}\n    retry: {max_attempts: 3, backoff: exponential}\n  - id: review\n    action: Route for review.\n    depends_on: [verify]\n    conditions: [\"${{ steps.classify.outputs.doc_type != 'other' }}\"]\n    compliance: {requires_approval: true}\nerror_handling: {on_step_failure: escalate, escalation_target: cco}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("workflows/onboard.yaml"),
            "name: onboard\ndescription: Welcome a new user.\nsteps:\n  - {skill: greet, prompt: Say hello.}\n  - {skill: explain, prompt: Explain the rules.}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("workflows/release.md"),
            "---\nname: release\ndescription: How we cut a release.\n---\n1. Tag.\n",
        )
        .unwrap();
        std::fs::write(root.join("workflows/bad.yaml"), "name: bad\nsteps:\n  - {id: a, action: first, depends_on: [b]}\n  - {id: b, action: second, depends_on: [a]}\n").unwrap();
        let (ws, warnings) = discover(root);
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["onboard", "release", "review"]);
        assert_eq!(
            warnings,
            vec!["workflows/bad.yaml: the steps a, b depend on each other in a cycle"]
        );

        let review = &ws[2];
        assert_eq!(review.inputs[1].default, Some(json!("all")));
        assert!(!review.inputs[1].required && review.inputs[0].required);
        assert_eq!(review.outputs, vec!["report"]);
        let order: Vec<&str> = review
            .ordered_steps()
            .unwrap()
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(order, vec!["classify", "verify", "review", "report"]);
        let verify = &review.steps[2];
        assert_eq!(
            (verify.max_attempts, verify.backoff),
            (3, Backoff::Exponential)
        );
        assert_eq!(verify.agent.as_deref(), Some("fact-checker"));
        assert!(review.steps[3].requires_approval);
        assert_eq!(review.on_step_failure, OnFailure::Escalate);
        assert_eq!(review.escalation_target.as_deref(), Some("cco"));

        let onboard = &ws[0];
        assert_eq!(onboard.steps[1].depends_on, vec!["step-1"]);
        assert_eq!(onboard.steps[1].action, "Explain the rules.");
        assert_eq!(onboard.steps[1].skill.as_deref(), Some("explain"));
        assert!(ws[1].is_narrative());

        let section = prompt_section(&ws);
        assert!(section.contains("- review: Review a document. Inputs: document, framework?.\n"));
        assert!(section.contains("- release: How we cut a release. (a written procedure to read in workflows/release.md)\n"));
    }

    #[test]
    fn expressions_resolve_against_inputs_and_step_outputs() {
        let ctx = json!({
            "inputs": {"framework": "finra", "limit": 3},
            "steps": {"classify": {"outputs": {"doc_type": "filing", "rules": ["a", "b"], "count": 2, "flag": false}}}
        });
        assert_eq!(
            resolve("steps.classify.outputs.doc_type", &ctx),
            json!("filing")
        );
        assert_eq!(
            substitute(&json!("${{ steps.classify.outputs.rules }}"), &ctx),
            json!(["a", "b"])
        );
        assert_eq!(
            substitute(
                &json!({"note": "Found ${{ steps.classify.outputs.count }} for ${{ inputs.framework }}"}),
                &ctx
            ),
            json!({"note": "Found 2 for finra"})
        );
        assert!(holds(
            "${{ steps.classify.outputs.doc_type != 'other' }}",
            &ctx
        ));
        assert!(holds("${{ steps.classify.outputs.count > 1 }}", &ctx));
        assert!(!holds("${{ steps.classify.outputs.count >= 3 }}", &ctx));
        assert!(holds("${{ inputs.limit == 3 }}", &ctx));
        assert!(holds("${{ steps.classify.outputs.rules }}", &ctx));
        assert!(!holds("${{ steps.classify.outputs.flag }}", &ctx));
        assert!(!holds("${{ steps.missing.outputs.x }}", &ctx));
        assert!(holds(
            "${{ steps.classify.outputs.doc_type == \"filing\" }}",
            &ctx
        ));
        assert!(!holds("${{ 'a=b' == 'c' }}", &ctx));
    }
}
