//! Tools offered to the model and their implementations.
//!
//! Tool definitions are part of the cached prompt prefix, so their text and
//! order must not change between tasks.

mod declared;
mod exec;
mod image_read;
mod memory;
mod patch;
mod read_file;
mod skill;
mod stdin;
mod str_replace;
mod todo;
mod write_file;

use std::time::Duration;

use gitagent_loader::{AgentDef, DeclaredTool, EditTool, MemoryCfg, Skill, ToolsCfg};
use gitagent_provider::{ImageData, ToolCall, ToolSpec};
use gitagent_record::Counters;
use gitagent_shaper::Shaper;
use gitagent_terminal::Terminal;
use serde_json::{json, Value};

pub mod naming;

pub use declared::spec as declared_spec;
pub use memory::spec as memory_spec;

pub const EXEC: &str = "exec";
pub const WRITE_STDIN: &str = "write_stdin";
pub const READ_FILE: &str = "read_file";
pub const WRITE_FILE: &str = "write_file";
pub const STR_REPLACE: &str = "str_replace";
pub const APPLY_PATCH: &str = "apply_patch";
pub const IMAGE_READ: &str = "image_read";
pub const TODO: &str = "todo";
pub const TASK_COMPLETE: &str = "task_complete";
pub const SKILL: &str = "skill";
pub const SUBAGENT: &str = "subagent";
pub const MEMORY: &str = "memory";

/// Upper bound on how long one call may wait.
pub const MAX_YIELD_MS: u64 = 600_000;
/// Default wait after sending input with `write_stdin`.
pub const STDIN_YIELD_MS: u64 = 2_000;

/// The tools for this run, in a fixed order. The set depends only on the
/// profile, never on the task.
pub fn specs(cfg: &ToolsCfg, edit_tool: EditTool) -> Vec<ToolSpec> {
    let mut tools = vec![exec_spec(cfg.write_stdin)];
    if cfg.write_stdin {
        tools.push(ToolSpec {
            name: WRITE_STDIN.into(),
            description: "Send input to a session whose command is still running, then return the \
                new output. With empty `chars` it just waits for more output. Write control keys \
                as escapes: \"\\x03\" is Ctrl-C (interrupt), \"\\x04\" is Ctrl-D (end of input), \
                \"\\e\" is Escape, \"\\n\" is Enter."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {"type": "integer", "description": "The session to write to, as reported by exec (1 is the main shell)."},
                    "chars": {"type": "string", "description": "Text and keys to send; empty to only wait for output."},
                    "yield_ms": {"type": "integer", "description": "How long to wait for output, in milliseconds (default 2000 after input, 10000 when only waiting; max 600000)."}
                },
                "required": ["session_id"]
            }),
        });
    }
    tools.push(ToolSpec {
        name: READ_FILE.into(),
        description: "Read a text file. Returns numbered lines.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path, or relative to the main shell's current directory."},
                "offset": {"type": "integer", "description": "First line to show, starting at 1 (default 1)."},
                "limit": {"type": "integer", "description": "Maximum number of lines to show (default 2000)."}
            },
            "required": ["path"]
        }),
    });
    tools.push(ToolSpec {
        name: WRITE_FILE.into(),
        description: "Create a file with the given content, or replace a file completely. \
            Parent directories are created. To change part of an existing file, edit it with \
            a command instead."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path, or relative to the main shell's current directory."},
                "content": {"type": "string", "description": "The full content of the file."}
            },
            "required": ["path", "content"]
        }),
    });
    tools.push(edit_spec(edit_tool));
    if cfg.image_read {
        tools.push(ToolSpec {
            name: IMAGE_READ.into(),
            description: "Look at an image file (PNG, JPEG, GIF or WebP, up to about 3.7 MB).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path, or relative to the main shell's current directory."}
                },
                "required": ["path"]
            }),
        });
    }
    if cfg.todo {
        tools.push(ToolSpec {
            name: TODO.into(),
            description: "Keep a checklist for this task. Each call replaces the whole list; \
                use it to track requirements and what is left to verify."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                            },
                            "required": ["text"]
                        }
                    }
                },
                "required": ["items"]
            }),
        });
    }
    tools.push(ToolSpec {
        name: TASK_COMPLETE.into(),
        description: "Finish the task. Call this only after you have verified that every \
            requirement is met."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string", "description": "One or two sentences on what was done and how it was verified."}
            }
        }),
    });
    tools
}

fn edit_spec(edit_tool: EditTool) -> ToolSpec {
    match edit_tool {
        EditTool::StrReplace => ToolSpec {
            name: STR_REPLACE.into(),
            description: "Edit a file by replacing an exact piece of text. `old_str` must match \
                the file exactly, including indentation, and appear once (or set replace_all). \
                Include a few surrounding lines to make it unique."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path, or relative to the main shell's current directory."},
                    "old_str": {"type": "string", "description": "The exact text to replace."},
                    "new_str": {"type": "string", "description": "The text to put in its place."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one."}
                },
                "required": ["path", "old_str", "new_str"]
            }),
        },
        EditTool::ApplyPatch => ToolSpec {
            name: APPLY_PATCH.into(),
            description: "Add, update, move or delete files with one patch:\n\
                *** Begin Patch\n\
                *** Add File: path/new.txt\n\
                +each line of the new file starts with +\n\
                *** Update File: path/app.py\n\
                @@ optional line to search for first, e.g. a function header\n\
                 unchanged context line (starts with a space)\n\
                -removed line\n\
                +added line\n\
                *** Delete File: path/old.txt\n\
                *** End Patch\n\
                Add `*** Move to: new/path` right after an Update File line to rename. Give about \
                three lines of context around each change; use several @@ sections for changes far \
                apart. Paths are relative to the main shell's current directory."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patch": {"type": "string", "description": "The whole patch, from *** Begin Patch to *** End Patch."}
                },
                "required": ["patch"]
            }),
        },
    }
}

fn exec_spec(sessions: bool) -> ToolSpec {
    let mut props = json!({
        "cmd": {
            "type": "string",
            "description": "The command to run. Multi-line scripts and heredocs are fine."
        },
        "yield_ms": {
            "type": "integer",
            "description": "How long to wait for the command to finish, in milliseconds (default 10000, max 600000). Use a longer wait for builds, installs and test suites."
        }
    });
    let description = if sessions {
        props["background"] = json!({
            "type": "boolean",
            "description": "Run in a new background session instead of the main shell (for servers, REPLs and watchers). It starts in the main shell's directory with its exported variables."
        });
        "Run a shell command. By default it runs in the main shell (session 1), which keeps \
        its working directory, environment variables and shell state between calls. Returns \
        the output (stdout and stderr together) and the exit code. If the command is still \
        running after `yield_ms`, returns the output so far and the session id: use \
        write_stdin to wait for more output, send input or interrupt it. Running another \
        command in the main shell first interrupts a command still running there."
    } else {
        "Run a shell command in a persistent bash session. The working directory, \
        environment variables and shell state carry over between calls. Returns the output \
        (stdout and stderr together) and the exit code. If the command is still running \
        after `yield_ms`, returns the output so far; the next `exec` call first interrupts \
        it with Ctrl-C."
    };
    ToolSpec {
        name: EXEC.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": props,
            "required": ["cmd"]
        }),
    }
}

/// What a tool call produced, ready to be sent back to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// Set by `task_complete`: the agent asked to finish.
    pub complete: bool,
    pub images: Vec<ImageData>,
    /// False when a command was still running when its output was returned.
    pub finished: bool,
    /// What this result becomes if it is later hidden to save context.
    pub stub: Option<String>,
}

impl ToolOutput {
    fn ok(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: false,
            complete: false,
            images: Vec::new(),
            finished: true,
            stub: None,
        }
    }

    fn error(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: true,
            complete: false,
            images: Vec::new(),
            finished: true,
            stub: None,
        }
    }
}

pub struct ToolContext<'a> {
    pub terminal: &'a mut Terminal,
    pub shaper: &'a mut Shaper,
    pub counters: &'a mut Counters,
    pub tools: &'a ToolsCfg,
    pub edit_tool: EditTool,
    pub skills: &'a [Skill],
    /// The project's memory, when the `memory` tool is offered.
    pub memory: Option<&'a MemoryCfg>,
    /// Tools the agent declares in `tools/*.yaml`.
    pub declared: &'a [DeclaredTool],
    /// Connected MCP servers.
    pub mcp: Option<&'a gitagent_mcp::Servers>,
    pub default_yield_ms: u64,
    /// Time left in the run, used to cap waits.
    pub remaining: Option<Duration>,
    /// Shown in each command result when the time budget feature is on.
    pub minutes_left: Option<u64>,
    /// The model sees `.claude/` tool names (`tool_naming: sdk`): error messages
    /// name tools and arguments the way the model knows them.
    pub sdk_names: bool,
}

/// Longest wait for an MCP tool call.
const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// An MCP tool's result as a tool output.
fn mcp_output(out: gitagent_mcp::CallOutput) -> ToolOutput {
    let text = if out.text.is_empty() {
        "(no output)".to_string()
    } else {
        declared::cap(&out.text)
    };
    ToolOutput {
        images: out.images,
        ..if out.is_error {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        }
    }
}

/// Where tests are placed for grading; nothing may write there.
pub const PROTECTED_DIR: &str = "/tests";

pub async fn dispatch(call: &ToolCall, ctx: &mut ToolContext<'_>) -> ToolOutput {
    if writes_protected_dir(call, ctx.terminal.cwd()) {
        ctx.counters.inc("protected_writes_blocked");
        return ToolOutput::error(format!(
            "Refused: nothing may be written under {PROTECTED_DIR}."
        ));
    }
    let result = match call.name.as_str() {
        EXEC => exec::run(call, ctx).await,
        WRITE_STDIN if ctx.tools.write_stdin => stdin::run(call, ctx).await,
        READ_FILE => {
            if let Ok(path) = str_arg(call, "path") {
                if ctx.shaper.mentions_spill(path) {
                    ctx.counters.inc("spill_reads");
                }
            }
            read_file::run(call, ctx.terminal.cwd())
        }
        WRITE_FILE => write_file::run(call, ctx.terminal.cwd()),
        STR_REPLACE if ctx.edit_tool == EditTool::StrReplace => run_str_replace(call, ctx),
        APPLY_PATCH if ctx.edit_tool == EditTool::ApplyPatch => run_apply_patch(call, ctx),
        SKILL if !ctx.skills.is_empty() => skill::run(call, ctx.skills),
        MEMORY if ctx.memory.is_some() => {
            memory::run(call, ctx.memory.expect("checked above"), ctx.counters)
        }
        IMAGE_READ if ctx.tools.image_read => image_read::run(call, ctx.terminal.cwd()),
        TODO if ctx.tools.todo => todo::run(call),
        TASK_COMPLETE => Ok(ToolOutput {
            complete: true,
            ..ToolOutput::ok("Task marked complete.")
        }),
        name if ctx.declared.iter().any(|t| t.name == name) => {
            ctx.counters.inc_labelled("declared_tool_calls", name);
            let tool = ctx.declared.iter().find(|t| t.name == name);
            declared::run(call, tool.expect("checked above"), ctx.remaining).await
        }
        name if ctx.mcp.is_some_and(|m| m.has(name)) => {
            ctx.counters.inc_labelled("mcp_tool_calls", name);
            let servers = ctx.mcp.expect("checked above");
            let timeout = ctx
                .remaining
                .map_or(MCP_CALL_TIMEOUT, |r| r.min(MCP_CALL_TIMEOUT));
            Ok(match servers.call(name, &call.arguments, timeout).await {
                Ok(out) => mcp_output(out),
                Err(why) => ToolOutput::error(format!("Error: {}", declared::cap(&why))),
            })
        }
        other => {
            ctx.counters.inc("malformed_calls");
            let mut offered = specs(ctx.tools, ctx.edit_tool);
            if ctx.sdk_names {
                offered = naming::sdk_specs(offered);
            }
            let mut names: Vec<String> = offered.into_iter().map(|t| t.name).collect();
            names.extend(ctx.declared.iter().map(|t| t.name.clone()));
            names.extend(ctx.mcp.into_iter().flat_map(|m| m.specs()).map(|t| t.name));
            Ok(ToolOutput::error(format!(
                "Unknown tool `{other}`. Available tools: {}.",
                names.join(", ")
            )))
        }
    };
    result.unwrap_or_else(|e| {
        if e.is::<ArgError>() {
            ctx.counters.inc("malformed_calls");
            let shown = if ctx.sdk_names {
                gitagent_loader::sdk::sdk_name(&call.name).to_string()
            } else {
                call.name.clone()
            };
            return ToolOutput::error(format!(
                "Invalid arguments for {shown}: {e:#}. {}",
                expected_arguments(&call.name, ctx.tools, ctx.edit_tool, ctx.sdk_names)
            ));
        }
        ToolOutput::error(format!("{e:#}"))
    })
}

/// One line describing a tool's arguments, e.g.
/// "Expected arguments: cmd (string, required), yield_ms (integer)."
fn expected_arguments(tool: &str, cfg: &ToolsCfg, edit_tool: EditTool, sdk_names: bool) -> String {
    let mut offered = specs(cfg, edit_tool);
    let tool = if sdk_names {
        offered = naming::sdk_specs(offered);
        gitagent_loader::sdk::sdk_name(tool).to_string()
    } else {
        tool.to_string()
    };
    let Some(spec) = offered.into_iter().find(|t| t.name == tool) else {
        return String::new();
    };
    let required: Vec<&str> = spec.input_schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let props = spec.input_schema["properties"].as_object();
    let parts: Vec<String> = props
        .into_iter()
        .flatten()
        .map(|(name, schema)| {
            let ty = schema["type"].as_str().unwrap_or("value");
            if required.contains(&name.as_str()) {
                format!("{name} ({ty}, required)")
            } else {
                format!("{name} ({ty})")
            }
        })
        .collect();
    format!("Expected arguments: {}.", parts.join(", "))
}

fn run_str_replace(call: &ToolCall, ctx: &mut ToolContext<'_>) -> anyhow::Result<ToolOutput> {
    let path = ctx.terminal.cwd().join(str_arg(call, "path")?);
    let old = str_arg(call, "old_str")?;
    let new = str_arg(call, "new_str")?;
    let all = bool_arg(call, "replace_all")?;
    Ok(match str_replace::edit(&path, old, new, all) {
        Ok(e) => {
            if e.fallback {
                ctx.counters.inc("str_replace_fallbacks");
            }
            ToolOutput::ok(e.message)
        }
        Err(msg) => ToolOutput::error(msg),
    })
}

fn run_apply_patch(call: &ToolCall, ctx: &mut ToolContext<'_>) -> anyhow::Result<ToolOutput> {
    Ok(
        match patch::apply(str_arg(call, "patch")?, ctx.terminal.cwd()) {
            Ok(done) => ToolOutput::ok(format!("Patch applied: {}.", done.join(", "))),
            Err(msg) => {
                ctx.counters.inc("patch_failures");
                ToolOutput::error(format!("Patch not applied (no files changed): {msg}"))
            }
        },
    )
}

/// The `skill` tool, offered when the config has skills. Added after the
/// fixed tools so the profile's tools keep their order.
pub fn skill_spec(skills: &[Skill]) -> ToolSpec {
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    ToolSpec {
        name: SKILL.into(),
        description: "Load the full instructions of a skill listed in the system prompt.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "enum": names, "description": "The skill's name."}
            },
            "required": ["name"]
        }),
    }
}

/// True when the call would write beneath `/tests`: file tools aimed there,
/// patches touching it, or shell commands that name it and write something.
fn writes_protected_dir(call: &ToolCall, cwd: &std::path::Path) -> bool {
    let under = |p: &str| {
        let full = cwd.join(p);
        full.starts_with(PROTECTED_DIR)
    };
    let arg = |k: &str| call.arguments.get(k).and_then(Value::as_str);
    match call.name.as_str() {
        WRITE_FILE | STR_REPLACE => arg("path").is_some_and(under),
        APPLY_PATCH => arg("patch").is_some_and(|p| {
            p.lines().any(|l| {
                [
                    "*** Add File: ",
                    "*** Update File: ",
                    "*** Delete File: ",
                    "*** Move to: ",
                ]
                .iter()
                .any(|m| l.strip_prefix(m).is_some_and(|f| under(f.trim())))
            })
        }),
        EXEC | WRITE_STDIN => {
            let text = arg("cmd").or_else(|| arg("chars")).unwrap_or("");
            let mentions = regex_lite_find(text);
            mentions
                && (WRITE_WORDS.iter().any(|w| contains_word(text, w))
                    || redirect_targets(text).any(|t| {
                        // After a `cd`, a relative target could resolve anywhere.
                        regex_lite_find(t)
                            || under(t)
                            || (!t.starts_with('/') && contains_word(text, "cd"))
                    }))
        }
        _ => false,
    }
}

const WRITE_WORDS: &[&str] = &[
    "tee", "cp", "mv", "rm", "mkdir", "touch", "ln", "chmod", "chown", "rsync", "install", "sed",
    "truncate", "dd", "unzip", "tar",
];

/// The files a shell command redirects output into (`> f`, `>> f`, `&> f`,
/// `>| f`). Descriptor copies such as `2>&1` are not files and are skipped, so
/// `ls /tests 2>/dev/null` reads /tests without counting as a write to it.
fn redirect_targets(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices('>').filter_map(move |(i, _)| {
        if text[..i].ends_with('>') {
            return None; // second `>` of `>>`, handled with the first
        }
        let rest = text[i + 1..].trim_start_matches(['>', '|']);
        if rest.starts_with('&') || rest.starts_with('(') {
            return None; // `2>&1`, `>&2`, or process substitution `>(...)`
        }
        let rest = rest.trim_start();
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | ')'))
            .unwrap_or(rest.len());
        Some(rest[..end].trim_matches(['"', '\'']))
    })
}

/// Does `text` name `/tests` as a path (not, say, `/app/tests`)?
fn regex_lite_find(text: &str) -> bool {
    text.match_indices(PROTECTED_DIR).any(|(i, _)| {
        let before = text[..i].chars().last();
        let after = text[i + PROTECTED_DIR.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~'))
            && !after.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
    })
}

fn contains_word(text: &str, word: &str) -> bool {
    if word == ">" {
        return text.contains('>');
    }
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .any(|w| w == word)
}

/// The `subagent` tool, offered when the profile enables sub-agents.
pub fn subagent_spec(agents: &[AgentDef]) -> ToolSpec {
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    let list: Vec<String> = agents
        .iter()
        .map(|a| format!("- {}: {}", a.name, a.description))
        .collect();
    ToolSpec {
        name: SUBAGENT.into(),
        description: format!(
            "Start a sub-agent for a self-contained piece of work. It starts with a fresh context \
             and its own shell in your current directory, sees only `prompt` (so include every path \
             and detail it needs), and returns only its final summary. Processes it starts stop when \
             it finishes. Agents:\n{}",
            list.join("\n")
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "A few words on what the sub-agent will do."},
                "prompt": {"type": "string", "description": "The complete request for the sub-agent."},
                "agent": {"type": "string", "enum": names, "description": "Which agent to use (default: general)."}
            },
            "required": ["description", "prompt"]
        }),
    }
}

/// Invalid tool arguments from the model.
#[derive(Debug)]
struct ArgError(String);

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ArgError {}

fn str_arg<'a>(call: &'a ToolCall, name: &str) -> anyhow::Result<&'a str> {
    match call.arguments.get(name) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(ArgError(format!("`{name}` must be a string")).into()),
        None => Err(ArgError(format!("missing required argument `{name}`")).into()),
    }
}

fn bool_arg(call: &ToolCall, name: &str) -> anyhow::Result<bool> {
    match call.arguments.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "true" || s == "false" => Ok(s == "true"),
        Some(_) => Err(ArgError(format!("`{name}` must be true or false")).into()),
    }
}

fn int_arg(call: &ToolCall, name: &str) -> anyhow::Result<Option<u64>> {
    match call.arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .map(Some)
            .ok_or_else(|| ArgError(format!("`{name}` must be a non-negative integer")).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_under_tests_are_detected() {
        let call = |name: &str, args: serde_json::Value| ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: args.as_object().unwrap().clone(),
        };
        let cwd = std::path::Path::new("/app");
        let blocked = |c: ToolCall| writes_protected_dir(&c, cwd);
        assert!(blocked(call(
            WRITE_FILE,
            json!({"path": "/tests/test_x.py", "content": ""})
        )));
        assert!(
            blocked(call(
                WRITE_FILE,
                json!({"path": "../tests/a", "content": ""})
            )) == std::path::Path::new("/app/../tests/a").starts_with("/tests")
        );
        assert!(!blocked(call(
            WRITE_FILE,
            json!({"path": "tests/test_x.py", "content": ""})
        )));
        assert!(blocked(call(
            EXEC,
            json!({"cmd": "mkdir -p /tests && echo x > /tests/a"})
        )));
        assert!(blocked(call(EXEC, json!({"cmd": "cp out.txt /tests/"}))));
        assert!(!blocked(call(EXEC, json!({"cmd": "ls /tests"}))));
        // Reading /tests with an error redirect elsewhere is not a write to it.
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "pip3 list 2>/dev/null; ls /tests 2>&1"})
        )));
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "cat /app/feal.py; ls /tests 2>/dev/null"})
        )));
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "ls /tests > /tmp/list.txt"})
        )));
        assert!(blocked(call(EXEC, json!({"cmd": "echo x > /tests/a"}))));
        assert!(blocked(call(
            EXEC,
            json!({"cmd": "echo x >>/tests/a 2>&1"})
        )));
        assert!(blocked(call(EXEC, json!({"cmd": "make &> /tests/log"}))));
        assert!(blocked(call(
            EXEC,
            json!({"cmd": "cd / && echo x > tests/a; ls /tests"})
        )));
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "grep '>' seq.fasta; ls /tests 2>/dev/null"})
        )));
        assert!(blocked(call(
            EXEC,
            json!({"cmd": "echo x > \"/tests/a b\""})
        )));
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "pytest /app/tests > log.txt"})
        )));
        assert!(!blocked(call(
            EXEC,
            json!({"cmd": "echo hi > /tmp/testsuite"})
        )));
        assert!(blocked(call(
            APPLY_PATCH,
            json!({"patch": "*** Begin Patch\n*** Add File: /tests/x\n+a\n*** End Patch"})
        )));
    }

    #[test]
    fn specs_are_stable_and_follow_the_profile() {
        let on = ToolsCfg {
            write_stdin: true,
            image_read: true,
            todo: true,
            subagents: true,
            subagent_depth: 1,
            max_concurrent_subagents: 4,
        };
        let off = ToolsCfg {
            write_stdin: false,
            image_read: false,
            todo: false,
            subagents: false,
            subagent_depth: 1,
            max_concurrent_subagents: 4,
        };
        let sr = EditTool::StrReplace;
        let a = serde_json::to_string(&specs(&on, sr)).unwrap();
        assert_eq!(a, serde_json::to_string(&specs(&on, sr)).unwrap());
        let names = |c, e| specs(c, e).into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(
            names(&on, sr),
            [
                EXEC,
                WRITE_STDIN,
                READ_FILE,
                WRITE_FILE,
                STR_REPLACE,
                IMAGE_READ,
                TODO,
                TASK_COMPLETE
            ]
        );
        assert_eq!(
            names(&off, EditTool::ApplyPatch),
            [EXEC, READ_FILE, WRITE_FILE, APPLY_PATCH, TASK_COMPLETE]
        );
        assert!(specs(&on, sr)[0].input_schema["properties"]
            .get("background")
            .is_some());
        assert!(specs(&off, sr)[0].input_schema["properties"]
            .get("background")
            .is_none());
    }
}
