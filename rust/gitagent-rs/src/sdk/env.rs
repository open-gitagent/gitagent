//! Load `~/.gitagent/.env` then `<agent-dir>/.env` into the process environment
//! (agent-dir wins). Mirrors the TS CLI's env precedence so API keys +
//! GITAGENT_MODEL_BASE_URL are picked up.

use std::path::Path;

pub fn load_env(agent_dir: &Path) {
    load_user_env();
    load_env_file(&agent_dir.join(".env"));
}

/// Load only `~/.gitagent/.env` — for agent dirs that aren't trusted (a cloned
/// repository must not be able to set API endpoints or keys).
pub fn load_user_env() {
    if let Some(home) = std::env::var_os("HOME") {
        load_env_file(&Path::new(&home).join(".gitagent").join(".env"));
    }
}

fn load_env_file(path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        // Strip a matching pair of surrounding quotes, if present.
        let v = if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
        {
            &v[1..v.len() - 1]
        } else {
            v
        };
        std::env::set_var(k.trim(), v);
    }
}
