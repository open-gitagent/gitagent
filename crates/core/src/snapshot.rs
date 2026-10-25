//! Environment snapshot (BLUEPRINT §5): one probe at startup, run in its own
//! process (the agent's shell is untouched), added to the first user message.
//! It saves the turns the agent would otherwise spend looking around.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CHARS: usize = 4000;

const PROBE: &str = r#"
echo "directory: $(pwd)"
echo "user: $(id -un 2>/dev/null || whoami 2>/dev/null)"
echo "system: $(uname -srm 2>/dev/null)"
mem=$(awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{if (t) printf "%d MB total, %d MB available", t/1024, a/1024}' /proc/meminfo 2>/dev/null)
echo "cpus: $(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null)  memory: ${mem:-unknown}"
echo
echo "$ ls -la"
ls -la 2>&1 | head -40
echo
echo "tools:"
for t in python3 pip3 node npm go rustc cargo gcc g++ java make cmake git docker psql; do
  if command -v "$t" >/dev/null 2>&1; then
    case "$t" in
      go) v=$(go version 2>&1 | head -n 1) ;;
      java) v=$(java -version 2>&1 | head -n 1) ;;
      *) v=$("$t" --version 2>&1 | head -n 1) ;;
    esac
    echo "  $t: $v"
  else
    echo "  $t: absent"
  fi
done
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo
  echo "$ git status --short --branch"
  git status --short --branch 2>&1 | head -20
fi
found=""
for f in README* Makefile package.json pyproject.toml requirements.txt setup.py Cargo.toml go.mod; do
  [ -e "$f" ] && found="$found $f"
done
[ -n "$found" ] && { echo; echo "project files:$found"; }
true
"#;

/// Runs the probe in `dir`. Returns `None` if it fails or takes too long.
pub async fn probe(dir: &Path, remove_env: &[&str]) -> Option<String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(PROBE)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    for key in remove_env {
        cmd.env_remove(key);
    }
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > MAX_CHARS {
        let cut: String = text.chars().take(MAX_CHARS).collect();
        return Some(format!("{cut}\n[snapshot cut at {MAX_CHARS} characters]"));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:\n").unwrap();
        let snap = probe(dir.path(), &[]).await.expect("snapshot");
        assert!(snap.contains("directory: "));
        assert!(snap.contains("$ ls -la"));
        assert!(snap.contains("Makefile"));
        assert!(snap.contains("project files: Makefile"));
        assert!(snap.contains("  git: "));
    }

    #[tokio::test]
    async fn missing_directory_gives_no_snapshot() {
        assert!(probe(Path::new("/nonexistent/dir"), &[]).await.is_none());
    }
}
