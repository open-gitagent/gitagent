//! `gitagent __sandbox --write <dir>... -- <program> [args...]`: applies the
//! sandbox to this process, then replaces it with the program (the agent's
//! shell). Used only internally, when the sandbox is on.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

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
    match gitagent_sandbox::apply(&Policy { writable }) {
        Ok(Status::Enforced) => {}
        Ok(Status::Partial(note) | Status::Unsupported(note)) => {
            eprintln!("gitagent: sandbox limited: {note}");
        }
        Err(e) => {
            eprintln!("gitagent: sandbox failed: {e:#}");
            return ExitCode::from(2);
        }
    }
    let err = Command::new(&program).args(rest).exec();
    eprintln!("gitagent __sandbox: cannot start {program:?}: {err}");
    ExitCode::from(127)
}
