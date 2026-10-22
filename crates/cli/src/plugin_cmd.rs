//! `gitagent plugin <install|list|remove|enable|disable|init> …`, with
//! `-d/--dir` for the agent directory. Changes to agent.yaml's `plugins:`
//! mapping are made in the text, so comments and the rest of the file stay
//! as they were.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use gitagent_loader::manifest::{git_clone, is_git_source};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "gitagent plugin", about = "Manage an agent's plugins.")]
struct PluginCli {
    /// The agent directory (default: the current directory).
    #[arg(short = 'd', long, global = true)]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install a plugin from a git URL (into .gitagent/plugins/) or a local
    /// path (copied into plugins/), and name it in agent.yaml.
    Install {
        source: String,
        /// The name in agent.yaml (default: the source's last path part).
        #[arg(long)]
        name: Option<String>,
        /// Replace an installed plugin.
        #[arg(long)]
        force: bool,
        /// Name it in agent.yaml as disabled.
        #[arg(long)]
        no_enable: bool,
    },
    /// List the plugins in plugins/, ~/.gitagent/plugins/ and .gitagent/plugins/.
    #[command(alias = "ls")]
    List,
    /// Delete a plugin from plugins/ or .gitagent/plugins/ and drop it from agent.yaml.
    #[command(alias = "rm")]
    Remove { name: String },
    /// Turn a plugin on in agent.yaml.
    Enable { name: String },
    /// Turn a plugin off in agent.yaml.
    Disable { name: String },
    /// Start a plugin in plugins/<name>/ and name it in agent.yaml.
    #[command(alias = "create")]
    Init { name: String },
}

pub(crate) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match PluginCli::try_parse_from(
        std::iter::once("gitagent plugin".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let root = cli.dir.unwrap_or_else(|| PathBuf::from("."));
    let root = std::path::absolute(&root).unwrap_or(root);
    let result = match cli.command {
        Command::Install {
            source,
            name,
            force,
            no_enable,
        } => install(&root, &source, name.as_deref(), force, !no_enable),
        Command::List => list(&root),
        Command::Remove { name } => remove(&root, &name),
        Command::Enable { name } => set_enabled(&root, &name, true),
        Command::Disable { name } => set_enabled(&root, &name, false),
        Command::Init { name } => init(&root, &name),
    };
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gitagent plugin: {e}");
            ExitCode::from(1)
        }
    }
}

fn install(
    root: &Path,
    source: &str,
    name: Option<&str>,
    force: bool,
    enable: bool,
) -> Result<String, String> {
    let base = source
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("plugin")
        .trim_end_matches(".git")
        .to_string();
    let name = name.unwrap_or(&base);
    let mut fields = vec![("enabled", Value::Bool(enable))];
    let dest = if is_git_source(source) {
        let dest = root.join(".gitagent/plugins").join(&base);
        if dest.join("plugin.yaml").is_file() && !force {
            // Already there: only agent.yaml changes.
        } else {
            if dest.exists() {
                std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
            }
            git_clone(source, &dest, None)?;
        }
        fields.push(("source", Value::String(source.to_string())));
        dest
    } else {
        let from = if Path::new(source).is_absolute() {
            PathBuf::from(source)
        } else {
            root.join(source)
        };
        if !from.join("plugin.yaml").is_file() {
            return Err(format!("{} has no plugin.yaml", from.display()));
        }
        let dest = root.join("plugins").join(name);
        if from.canonicalize().ok() != dest.canonicalize().ok() {
            if dest.exists() {
                if !force {
                    return Err(format!(
                        "{} exists (use --force to replace it)",
                        dest.display()
                    ));
                }
                std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
            }
            copy_dir(&from, &dest).map_err(|e| format!("copying {}: {e}", from.display()))?;
        }
        dest
    };
    if !dest.join("plugin.yaml").is_file() {
        return Err(format!("{} has no plugin.yaml", dest.display()));
    }
    edit_manifest(root, |text| set_entry(text, name, &fields))?;
    Ok(format!("installed {name} at {}", dest.display()))
}

fn list(root: &Path) -> Result<String, String> {
    let manifest = read_manifest(root).unwrap_or(Value::Null);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut scopes = vec![("local", root.join("plugins"))];
    if let Some(h) = &home {
        scopes.push(("global", h.join(".gitagent/plugins")));
    }
    scopes.push(("installed", root.join(".gitagent/plugins")));
    let mut lines = Vec::new();
    for (scope, dir) in scopes {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("plugin.yaml").is_file())
            .collect();
        entries.sort();
        for p in entries {
            let folder = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let m: Value = std::fs::read_to_string(p.join("plugin.yaml"))
                .ok()
                .and_then(|t| serde_saphyr::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let id = m["id"].as_str().unwrap_or(&folder);
            let enabled = manifest["plugins"]
                .get(id)
                .or(manifest["plugins"].get(&folder));
            let state = match enabled {
                None => "not in agent.yaml",
                Some(v) if v["enabled"] == Value::Bool(false) => "disabled",
                Some(_) => "enabled",
            };
            lines.push(format!(
                "{id} v{} ({scope}) {state} — {}",
                m["version"].as_str().unwrap_or("?"),
                m["description"].as_str().unwrap_or_default()
            ));
        }
    }
    if lines.is_empty() {
        Ok("No plugins.".into())
    } else {
        Ok(lines.join("\n"))
    }
}

fn remove(root: &Path, name: &str) -> Result<String, String> {
    let mut removed = None;
    for dir in [
        root.join("plugins").join(name),
        root.join(".gitagent/plugins").join(name),
    ] {
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            removed = Some(dir);
            break;
        }
    }
    let had_entry = read_manifest(root).is_ok_and(|m| m["plugins"].get(name).is_some());
    if removed.is_none() && !had_entry {
        return Err(format!("no plugin named {name:?}"));
    }
    edit_manifest(root, |text| remove_entry(text, name))?;
    Ok(match removed {
        Some(dir) => format!("removed {name} ({})", dir.display()),
        None => format!("removed {name} from agent.yaml"),
    })
}

fn set_enabled(root: &Path, name: &str, enabled: bool) -> Result<String, String> {
    edit_manifest(root, |text| {
        set_entry(text, name, &[("enabled", Value::Bool(enabled))])
    })?;
    Ok(format!(
        "{name} {}",
        if enabled { "enabled" } else { "disabled" }
    ))
}

fn init(root: &Path, name: &str) -> Result<String, String> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(format!(
            "the name {name:?} must be lowercase letters, digits and dashes"
        ));
    }
    let dir = root.join("plugins").join(name);
    if dir.join("plugin.yaml").is_file() {
        return Err(format!("{} already exists", dir.display()));
    }
    for sub in ["tools", "hooks", "skills"] {
        std::fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        dir.join("plugin.yaml"),
        format!(
            "id: {name}\nname: {name}\nversion: 0.1.0\ndescription: What this plugin adds.\nprovides:\n  tools: true     # tools/*.yaml\n  skills: true    # skills/<name>/SKILL.md\n  # prompt: PROMPT.md\n  # hooks:\n  #   pre_tool_use: [{{script: hooks/check.sh, description: Checks each call}}]\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("README.md"),
        format!("# {name}\n\nA gitagent plugin. Describe what it adds and how to set it up.\n"),
    )
    .map_err(|e| e.to_string())?;
    edit_manifest(root, |text| {
        set_entry(text, name, &[("enabled", Value::Bool(true))])
    })?;
    Ok(format!("created {}", dir.display()))
}

fn read_manifest(root: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(root.join("agent.yaml")).map_err(|e| e.to_string())?;
    serde_saphyr::from_str(&text).map_err(|e| e.to_string())
}

fn edit_manifest(root: &Path, change: impl FnOnce(&str) -> String) -> Result<(), String> {
    let path = root.join("agent.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e} (run `gitagent init` first)", path.display()))?;
    let changed = change(&text);
    if changed != text {
        std::fs::write(&path, changed).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

// ----------------------------------------------- agent.yaml's plugins block

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The lines of the top-level `plugins:` block: the key line's index and
/// the index after its last line.
fn plugins_block(lines: &[&str]) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "plugins:" || l.starts_with("plugins:"))?;
    let mut end = start + 1;
    while end < lines.len() {
        let l = lines[end];
        if !l.trim().is_empty() && !l.trim_start().starts_with('#') && indent_of(l) == 0 {
            break;
        }
        end += 1;
    }
    // Trailing blank lines belong to what follows.
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end))
}

/// The lines of one plugin's entry inside the block.
fn entry_lines(lines: &[&str], block: (usize, usize), name: &str) -> Option<(usize, usize)> {
    let (start, end) = block;
    let header = (start + 1..end).find(|&i| {
        let l = lines[i];
        indent_of(l) == 2 && l.trim().trim_end_matches(':').trim_end_matches(" {}") == name
            || (indent_of(l) == 2 && l.trim().starts_with(&format!("{name}:")))
    })?;
    let mut last = header + 1;
    while last < end && (lines[last].trim().is_empty() || indent_of(lines[last]) > 2) {
        last += 1;
    }
    while last > header + 1 && lines[last - 1].trim().is_empty() {
        last -= 1;
    }
    Some((header, last))
}

/// The parts of an inline mapping's inside, split on the commas outside
/// nested braces, brackets and quotes.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0usize, None::<char>, 0usize);
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '{' | '[' => depth += 1,
                '}' | ']' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    parts.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&s[start..]);
    parts
}

fn yaml_value(v: &Value) -> String {
    match v {
        Value::String(s) => {
            if s.is_empty()
                || s.contains([
                    '#', ':', '{', '}', '[', ']', ',', '&', '*', '!', '|', '>', '\'', '"', '%',
                    '@', '`',
                ])
            {
                format!("{s:?}")
            } else {
                s.clone()
            }
        }
        other => other.to_string(),
    }
}

/// agent.yaml's text with `fields` set on the plugin entry `name` (made
/// when missing).
fn set_entry(text: &str, name: &str, fields: &[(&str, Value)]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let block = plugins_block(&borrowed);
    let Some(block) = block else {
        if !lines.is_empty() && !lines.last().unwrap().trim().is_empty() {
            // Nothing to do: a new top-level key simply follows.
        }
        lines.push("plugins:".into());
        lines.push(format!("  {name}:"));
        for (k, v) in fields {
            lines.push(format!("    {k}: {}", yaml_value(v)));
        }
        return finish(lines);
    };
    match entry_lines(&borrowed, block, name) {
        Some((header, last)) => {
            // An inline `{...}` entry is rewritten as a block.
            let inline = lines[header].contains('{');
            let mut body: Vec<String> = if inline {
                let inner = lines[header]
                    .split_once('{')
                    .map(|(_, r)| {
                        r.trim_end()
                            .strip_suffix('}')
                            .unwrap_or(r.trim_end())
                            .to_string()
                    })
                    .unwrap_or_default();
                split_top_level(&inner)
                    .into_iter()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("    {s}"))
                    .collect()
            } else {
                lines[header + 1..last].to_vec()
            };
            for (k, v) in fields {
                let rendered = format!("    {k}: {}", yaml_value(v));
                match body
                    .iter()
                    .position(|l| l.trim_start().starts_with(&format!("{k}:")))
                {
                    Some(i) => body[i] = rendered,
                    None => body.push(rendered),
                }
            }
            let header_line = format!("  {name}:");
            lines.splice(header..last, std::iter::once(header_line).chain(body));
        }
        None => {
            let (_, end) = block;
            let mut new = vec![format!("  {name}:")];
            for (k, v) in fields {
                new.push(format!("    {k}: {}", yaml_value(v)));
            }
            lines.splice(end..end, new);
        }
    }
    finish(lines)
}

/// agent.yaml's text without the plugin entry `name` (and without an empty
/// `plugins:` key).
fn remove_entry(text: &str, name: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let Some(block) = plugins_block(&borrowed) else {
        return finish(lines);
    };
    if let Some((header, last)) = entry_lines(&borrowed, block, name) {
        lines.drain(header..last);
    }
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    if let Some((start, end)) = plugins_block(&borrowed) {
        let empty = (start + 1..end)
            .all(|i| borrowed[i].trim().is_empty() || borrowed[i].trim_start().starts_with('#'));
        if empty && borrowed[start].trim_end() == "plugins:" {
            lines.drain(start..end);
        }
    }
    finish(lines)
}

fn finish(lines: Vec<String>) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_set_and_removed_in_the_text() {
        let base = "name: a\nversion: 1.0.0  # keep me\ndescription: A.\n";
        let one = set_entry(base, "search", &[("enabled", Value::Bool(true))]);
        assert_eq!(one, "name: a\nversion: 1.0.0  # keep me\ndescription: A.\nplugins:\n  search:\n    enabled: true\n");
        let two = set_entry(
            &one,
            "linter",
            &[
                ("source", Value::String("https://x/y.git".into())),
                ("enabled", Value::Bool(false)),
            ],
        );
        assert!(two.ends_with("plugins:\n  search:\n    enabled: true\n  linter:\n    source: \"https://x/y.git\"\n    enabled: false\n"), "{two}");
        let off = set_entry(&two, "search", &[("enabled", Value::Bool(false))]);
        assert!(
            off.contains("  search:\n    enabled: false\n  linter:"),
            "{off}"
        );
        let inline = "name: a\nplugins:\n  search: {enabled: true, config: {region: eu}}\nruntime:\n  max_turns: 5\n";
        let changed = set_entry(inline, "search", &[("enabled", Value::Bool(false))]);
        assert!(
            changed.contains(
                "plugins:\n  search:\n    enabled: false\n    config: {region: eu}\nruntime:"
            ),
            "{changed}"
        );
        let gone = remove_entry(&two, "linter");
        assert!(
            gone.ends_with("plugins:\n  search:\n    enabled: true\n"),
            "{gone}"
        );
        let none = remove_entry(&gone, "search");
        assert_eq!(none, base);
        assert_eq!(remove_entry(base, "nope"), base);
    }
}
