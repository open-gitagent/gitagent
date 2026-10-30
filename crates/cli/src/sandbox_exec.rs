//! `gitagent __sandbox --write <dir>... -- <program> [args...]`: prepares the
//! sandbox (on Linux by restricting this process), then replaces this process
//! with the program (the agent's shell), started inside it. Used only
//! internally, when the sandbox is on.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::ExitCode;

use gitagent_sandbox::{Policy, Status};

pub(crate) fn run(args: &[OsString]) -> ExitCode {
    let mut writable = Vec::new();
    let mut rest = args.iter();
    let mut program = None;
    while let Some(a) = rest.next() {
        match a.to_str() {
            Some("--write") => {
                if let Some(dir) = rest.next() {
                    writable.push(PathBuf::from(dir));
                }
            }
            Some("--") => {
                program = rest.next().cloned();
                break;
            }
            _ => {
                eprintln!("gitagent __sandbox: unexpected argument {a:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(program) = program else {
        eprintln!("gitagent __sandbox: missing `-- <program>`");
        return ExitCode::from(2);
    };
    let sandbox = match gitagent_sandbox::prepare(&Policy { writable }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gitagent: sandbox failed: {e:#}");
            return ExitCode::from(2);
        }
    };
    if let Status::Partial(note) | Status::Unsupported(note) = &sandbox.status {
        eprintln!("gitagent: sandbox limited: {note}");
    }
    let err = sandbox.command(&program).args(rest).exec();
    eprintln!("gitagent __sandbox: cannot start {program:?}: {err}");
    ExitCode::from(127)
}
