//! Sandbox for the agent's shell (milestone M7), applied by the
//! `gitagent __sandbox` launcher before it executes the shell, so the
//! restrictions cover the shell and everything it starts while gitagent
//! itself stays unrestricted.
//!
//! On Linux:
//! - Landlock: writes are allowed only beneath the given directories (reading
//!   is not restricted).
//! - seccomp: a deny-list of system calls an agent never needs (mounting,
//!   kernel modules, rebooting, keyrings, BPF, ...); they fail with EPERM.
//!
//! Elsewhere it is not available and nothing is restricted.
//!
//! This crate is the only one allowed to use `unsafe`; it currently needs none.

use std::path::PathBuf;

use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Landlock and seccomp both applied.
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

pub fn available() -> bool {
    cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
pub fn apply(policy: &Policy) -> Result<Status> {
    let landlock = linux::landlock(policy);
    let seccomp = linux::seccomp();
    Ok(match (landlock, seccomp) {
        (Ok(true), Ok(())) => Status::Enforced,
        (Ok(false), Ok(())) => Status::Partial("Landlock is not supported by this kernel".into()),
        (Ok(_), Err(e)) => Status::Partial(format!("seccomp filter not applied: {e:#}")),
        (Err(e), Ok(())) => Status::Partial(format!("Landlock not applied: {e:#}")),
        (Err(a), Err(b)) => Status::Unsupported(format!("Landlock: {a:#}; seccomp: {b:#}")),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn apply(_policy: &Policy) -> Result<Status> {
    Ok(Status::Unsupported(
        "the sandbox needs Linux (Landlock and seccomp)".into(),
    ))
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::Command;

    /// Runs a sandboxed child (a re-run of this test binary) and reports
    /// what it could write.
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
            eprintln!("Landlock unavailable here; skipping: {text}");
            return;
        }
        assert!(text.contains("in-ok"), "{text}");
        assert!(text.contains("out-denied"), "{text}");
    }

    #[test]
    fn sandboxed_child() {
        let Ok(allowed) = std::env::var("GA_ALLOWED") else {
            return;
        };
        let status = apply(&Policy {
            writable: vec![allowed.into(), "/dev".into()],
        })
        .unwrap();
        println!("status {status:?}");
        let script = std::env::var("GA_SCRIPT").unwrap();
        let out = Command::new("sh").arg("-c").arg(script).output().unwrap();
        println!("{}", String::from_utf8_lossy(&out.stdout));
    }
}
