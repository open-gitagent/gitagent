//! `gitagent install [dir] [--force]`: fetches what an agent's agent.yaml
//! refers to. A git parent (`extends`) is cloned into `.gitagent/parent/`;
//! each dependency goes to its `mount` (or `.gitagent/deps/<name>`): a git
//! source is cloned at its `version`, a local one copied. What is already
//! there is left alone unless `--force` replaces it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use gitagent_loader::manifest::{dependency_mount, git_clone, is_git_source};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "gitagent install",
    about = "Fetch the parent agent (extends) and the dependencies an agent.yaml refers to."
)]
struct InstallCli {
    /// The agent directory (default: the current directory).
    dir: Option<PathBuf>,

    /// Replace what is already installed.
    #[arg(long)]
    force: bool,
}

pub(crate) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match InstallCli::try_parse_from(
        std::iter::once("gitagent install".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let root = cli.dir.unwrap_or_else(|| PathBuf::from("."));
    let root = std::path::absolute(&root).unwrap_or(root);
    let manifest = match std::fs::read_to_string(root.join("agent.yaml"))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_saphyr::from_str::<Value>(&t).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "gitagent install: {}: {e}",
                root.join("agent.yaml").display()
            );
            return ExitCode::from(2);
        }
    };
    let mut failed = false;
    let mut report = |what: &str, result: Result<String, String>| match result {
        Ok(note) => println!("{what}: {note}"),
        Err(e) => {
            eprintln!("{what}: {e}");
            failed = true;
        }
    };
    if let Some(src) = manifest["extends"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        report("parent", install_parent(&root, src, cli.force));
    }
    for dep in manifest["dependencies"].as_array().into_iter().flatten() {
        let Some((name, mount)) = dependency_mount(&root, dep) else {
            report("dependency", Err("has no `name`".into()));
            continue;
        };
        let source = dep["source"].as_str().map(str::trim).unwrap_or_default();
        let version = dep["version"].as_str().map(str::trim);
        report(
            &format!("dependency {name}"),
            install_dependency(&root, source, version, &mount, cli.force),
        );
    }
    ExitCode::from(u8::from(failed))
}

fn install_parent(root: &Path, source: &str, force: bool) -> Result<String, String> {
    if !is_git_source(source) {
        let path = if Path::new(source).is_absolute() {
            PathBuf::from(source)
        } else {
            root.join(source)
        };
        return if path.join("agent.yaml").is_file() {
            Ok(format!(
                "{} (a local directory; nothing to fetch)",
                path.display()
            ))
        } else {
            Err(format!("{} has no agent.yaml", path.display()))
        };
    }
    let dest = root.join(".gitagent/parent");
    if dest.join("agent.yaml").is_file() && !force {
        return Ok(format!(
            "already at {} (use --force to replace it)",
            dest.display()
        ));
    }
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    git_clone(source, &dest, None)?;
    Ok(format!("cloned {source} into {}", dest.display()))
}

fn install_dependency(
    root: &Path,
    source: &str,
    version: Option<&str>,
    mount: &Path,
    force: bool,
) -> Result<String, String> {
    if source.is_empty() {
        return Err("has no `source`".into());
    }
    if mount.join("agent.yaml").is_file() && !force {
        return Ok(format!(
            "already at {} (use --force to replace it)",
            mount.display()
        ));
    }
    if mount.exists() {
        std::fs::remove_dir_all(mount).map_err(|e| e.to_string())?;
    }
    if is_git_source(source) {
        // A SemVer range is used as a tag name, without its `^` or `~`.
        let tag = version.map(|v| v.trim_start_matches(['^', '~']).to_string());
        let cloned = match &tag {
            Some(t) => git_clone(source, mount, Some(t))
                .or_else(|_| git_clone(source, mount, Some(&format!("v{t}")))),
            None => git_clone(source, mount, None),
        };
        cloned?;
        return Ok(format!(
            "cloned {source}{} into {}",
            tag.map(|t| format!(" at {t}")).unwrap_or_default(),
            mount.display()
        ));
    }
    let from = if Path::new(source).is_absolute() {
        PathBuf::from(source)
    } else {
        root.join(source)
    };
    if !from.join("agent.yaml").is_file() {
        return Err(format!("{} has no agent.yaml", from.display()));
    }
    if from.canonicalize().ok() == mount.canonicalize().ok() {
        return Ok(format!(
            "{} is its own mount; nothing to copy",
            from.display()
        ));
    }
    copy_dir(&from, mount).map_err(|e| format!("copying {}: {e}", from.display()))?;
    Ok(format!(
        "copied {} into {}",
        from.display(),
        mount.display()
    ))
}

/// Copies a directory tree (symbolic links are copied as what they point
/// to; `.git` is left out).
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let target = to.join(&name);
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
