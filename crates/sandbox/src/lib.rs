//! Sandbox for the agent's shell (milestone M7), prepared by the
//! `gitagent __sandbox` launcher before it executes the shell, so the
//! restrictions cover the shell and everything it starts while gitagent
//! itself stays unrestricted.
//!
//! On Linux the launcher restricts itself, then executes the shell:
//! - Landlock: writes are allowed only beneath the given directories (reading
//!   is not restricted).
//! - seccomp: a deny-list of system calls an agent never needs (mounting,
//!   kernel modules, rebooting, keyrings, BPF, ...); they fail with EPERM.
//!
//! On macOS the launcher starts the shell through the system's
//! `/usr/bin/sandbox-exec` with a profile that allows writes only beneath the
//! given directories (reading is not restricted). There is no system-call
//! deny-list; an ordinary macOS user cannot mount, load kernel extensions or
//! reboot anyway.
//!
//! Elsewhere it is not available and nothing is restricted.
//!
//! This crate is the only one allowed to use `unsafe`; it currently needs none.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Everything this system's sandbox offers is applied.
    Enforced,
    /// Something was applied, with a note on what was not.
    Partial(String),
    /// Not available on this system.
    Unsupported(String),
}

/// Directories the sandboxed shell may write beneath.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub writable: Vec<PathBuf>,
}

/// A prepared sandbox: what was applied, and how to start a program under it.
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub status: Status,
    /// A command the program is started through (macOS), or empty.
    wrapper: Vec<OsString>,
}

impl Sandbox {
    /// A command that starts `program` inside the sandbox.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let Some((first, rest)) = self.wrapper.split_first() else {
            return Command::new(program);
        };
        let mut cmd = Command::new(first);
        cmd.args(rest).arg(program);
        // A process inside a macOS sandbox cannot enter another one; a
        // gitagent started by the agent sees this and stays in this one.
        cmd.env(INSIDE, "1");
        cmd
    }
}

/// Set for everything started inside a macOS sandbox.
const INSIDE: &str = "GITAGENT_IN_SANDBOX";

pub fn available() -> bool {
    cfg!(target_os = "linux") || (cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).exists())
}

/// On Linux this restricts the calling process (the launcher), so the
/// program it then executes inherits the restrictions.
#[cfg(target_os = "linux")]
pub fn prepare(policy: &Policy) -> Result<Sandbox> {
    let landlock = linux::landlock(policy);
    let seccomp = linux::seccomp();
    let status = match (landlock, seccomp) {
        (Ok(true), Ok(())) => Status::Enforced,
        (Ok(false), Ok(())) => Status::Partial("Landlock is not supported by this kernel".into()),
        (Ok(_), Err(e)) => Status::Partial(format!("seccomp filter not applied: {e:#}")),
        (Err(e), Ok(())) => Status::Partial(format!("Landlock not applied: {e:#}")),
        (Err(a), Err(b)) => Status::Unsupported(format!("Landlock: {a:#}; seccomp: {b:#}")),
    };
    Ok(Sandbox {
        status,
        wrapper: Vec::new(),
    })
}

/// On macOS nothing is applied to the calling process; the program is
/// started through `sandbox-exec` instead.
#[cfg(target_os = "macos")]
pub fn prepare(policy: &Policy) -> Result<Sandbox> {
    let unwrapped = |status| Sandbox {
        status,
        wrapper: Vec::new(),
    };
    if std::env::var_os(INSIDE).is_some() {
        return Ok(unwrapped(Status::Partial(
            "already inside a gitagent sandbox, whose limits still apply".into(),
        )));
    }
    if !Path::new(SANDBOX_EXEC).exists() {
        return Ok(unwrapped(Status::Unsupported(format!(
            "{SANDBOX_EXEC} is missing"
        ))));
    }
    Ok(Sandbox {
        status: Status::Enforced,
        wrapper: seatbelt_wrapper(&writable_paths(policy)),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn prepare(_policy: &Policy) -> Result<Sandbox> {
    Ok(Sandbox {
        status: Status::Unsupported(
            "the sandbox needs Linux (Landlock and seccomp) or macOS (sandbox-exec)".into(),
        ),
        wrapper: Vec::new(),
    })
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The writable directories that exist, each also under its resolved path:
/// macOS checks the real path, and `/tmp` and the temp folders are links
/// into `/private`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn writable_paths(policy: &Policy) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in policy.writable.iter().filter(|d| d.exists()) {
        let resolved = std::fs::canonicalize(dir).ok();
        for p in std::iter::once(dir.clone()).chain(resolved) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// `sandbox-exec` arguments for a profile that allows everything except
/// writing outside `dirs`. The paths are passed as parameters, so they need
/// no quoting inside the profile.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_wrapper(dirs: &[PathBuf]) -> Vec<OsString> {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    // With no directories, a bare `(allow file-write*)` would allow every write.
    if !dirs.is_empty() {
        profile.push_str("(allow file-write*");
        for i in 0..dirs.len() {
            profile.push_str(&format!(" (subpath (param \"W{i}\"))"));
        }
        profile.push_str(")\n");
    }
    let mut args: Vec<OsString> = vec![SANDBOX_EXEC.into(), "-p".into(), profile.into()];
    for (i, dir) in dirs.iter().enumerate() {
        let mut def = OsString::from(format!("W{i}="));
        def.push(dir);
        args.push("-D".into());
        args.push(def);
    }
    args
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;

    use anyhow::{Context, Result};
    use landlock::{
        path_beneath_rules, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI,
    };
    use seccompiler::{SeccompAction, SeccompFilter, TargetArch};

    use super::Policy;

    /// Returns false when the kernel does not support Landlock.
    pub fn landlock(policy: &Policy) -> Result<bool> {
        let abi = ABI::V5;
        let write = AccessFs::from_write(abi);
        let existing: Vec<&std::path::Path> = policy
            .writable
            .iter()
            .map(|p| p.as_path())
            .filter(|p| p.exists())
            .collect();
        let status = Ruleset::default()
            .handle_access(write)?
            .create()?
            .add_rules(path_beneath_rules(existing, write))?
            .restrict_self()
            .context("restricting the process")?;
        Ok(status.ruleset != RulesetStatus::NotEnforced)
    }

    pub fn seccomp() -> Result<()> {
        let mut denied: Vec<i64> = vec![
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_pivot_root,
            libc::SYS_swapon,
            libc::SYS_swapoff,
            libc::SYS_reboot,
            libc::SYS_kexec_load,
            libc::SYS_init_module,
            libc::SYS_finit_module,
            libc::SYS_delete_module,
            libc::SYS_bpf,
            libc::SYS_add_key,
            libc::SYS_keyctl,
            libc::SYS_request_key,
            libc::SYS_setns,
            libc::SYS_open_by_handle_at,
            libc::SYS_userfaultfd,
            libc::SYS_acct,
            libc::SYS_settimeofday,
            libc::SYS_clock_settime,
            libc::SYS_sethostname,
            libc::SYS_setdomainname,
            libc::SYS_quotactl,
            libc::SYS_vhangup,
        ];
        #[cfg(target_arch = "x86_64")]
        denied.extend([libc::SYS_iopl, libc::SYS_ioperm, libc::SYS_kexec_file_load]);
        // kexec_file_load; libc's musl table for aarch64 lacks the constant.
        #[cfg(target_arch = "aarch64")]
        denied.push(294);

        let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> =
            denied.into_iter().map(|nr| (nr, Vec::new())).collect();
        #[cfg(target_arch = "x86_64")]
        let arch = TargetArch::x86_64;
        #[cfg(target_arch = "aarch64")]
        let arch = TargetArch::aarch64;
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        )?;
        let program: seccompiler::BpfProgram = filter.try_into()?;
        seccompiler::apply_filter(&program)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a sandboxed child (a re-run of this test binary) and reports
    /// what it could write.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn writes_outside_the_allowed_directories_fail() {
        if std::env::var("GA_SANDBOX_CHILD").is_ok() {
            return;
        }
        let allowed = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let script = format!(
            "echo ok > {a}/in.txt && echo in-ok; echo x > {o}/out.txt 2>/dev/null && echo out-ok || echo out-denied",
            a = allowed.path().display(),
            o = other.path().display()
        );
        let exe = std::env::current_exe().unwrap();
        let out = Command::new(exe)
            .args(["--exact", "tests::sandboxed_child", "--nocapture"])
            .env("GA_SANDBOX_CHILD", "1")
            .env("GA_ALLOWED", allowed.path())
            .env("GA_SCRIPT", script)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains("status Unsupported") {
            eprintln!("sandbox unavailable here; skipping: {text}");
            return;
        }
        assert!(text.contains("status Enforced"), "{text}");
        assert!(text.contains("in-ok"), "{text}");
        assert!(text.contains("out-denied"), "{text}");
        assert!(!other.path().join("out.txt").exists(), "{text}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sandboxed_child() {
        let Ok(allowed) = std::env::var("GA_ALLOWED") else {
            return;
        };
        let sandbox = prepare(&Policy {
            writable: vec![allowed.into(), "/dev".into()],
        })
        .unwrap();
        println!("status {:?}", sandbox.status);
        let script = std::env::var("GA_SCRIPT").unwrap();
        let out = sandbox
            .command("sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&out.stdout));
        print!("{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn the_macos_profile_allows_writes_only_beneath_the_given_directories() {
        let dirs = [PathBuf::from("/work dir"), PathBuf::from("/a\"b")];
        let args = seatbelt_wrapper(&dirs);
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args[..2], ["/usr/bin/sandbox-exec", "-p"]);
        assert_eq!(
            args[2],
            "(version 1)\n(allow default)\n(deny file-write*)\n\
             (allow file-write* (subpath (param \"W0\")) (subpath (param \"W1\")))\n"
        );
        // Paths go in as parameters, never into the profile text.
        assert_eq!(args[3..], ["-D", "W0=/work dir", "-D", "W1=/a\"b"]);
    }

    #[test]
    fn with_no_writable_directories_the_macos_profile_allows_no_writes() {
        let args = seatbelt_wrapper(&[]);
        let profile = args[2].to_str().unwrap();
        assert!(!profile.contains("(allow file-write*"), "{profile}");
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn writable_paths_keep_existing_directories_and_add_their_real_paths() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let missing = root.path().join("missing");
        let paths = writable_paths(&Policy {
            writable: vec![link.clone(), missing, real.clone(), link.clone()],
        });
        let real = std::fs::canonicalize(&real).unwrap();
        assert!(paths.contains(&link) && paths.contains(&real), "{paths:?}");
        assert!(!paths.iter().any(|p| p.ends_with("missing")), "{paths:?}");
        assert_eq!(
            paths.len(),
            paths
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
    }

    #[test]
    fn without_a_wrapper_the_program_starts_directly() {
        let sandbox = Sandbox {
            status: Status::Enforced,
            wrapper: Vec::new(),
        };
        let cmd = sandbox.command("sh");
        assert_eq!(cmd.get_program(), "sh");
        assert_eq!(cmd.get_args().count(), 0);
    }
}
