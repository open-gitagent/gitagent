//! `gitagent validate`: checks an agent directory against the OpenGAP
//! format and exits with 1 when there are errors.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "gitagent validate",
    about = "Check an agent directory (agent.yaml, SOUL.md, skills, tools, hooks) against the OpenGAP format.",
    long_about = "Reports errors and warnings with the file and line. Exits with 1 when there \
                  are errors; warnings never fail. Running an agent is lenient (the same problems \
                  are only warnings then)."
)]
struct ValidateCli {
    /// The agent directory (default: the current directory).
    dir: Option<PathBuf>,

    /// The agent directory, as `opengap validate -d` takes it.
    #[arg(short = 'd', long = "dir", conflicts_with = "dir")]
    dir_flag: Option<PathBuf>,

    /// Also check the regulatory rules for the `compliance` section.
    #[arg(long)]
    compliance: bool,
}

pub(crate) fn run(args: &[std::ffi::OsString]) -> ExitCode {
    let cli = match ValidateCli::try_parse_from(
        std::iter::once("gitagent validate".into()).chain(args.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let dir = cli
        .dir_flag
        .or(cli.dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let report = gitagent_loader::validate::validate(&dir, cli.compliance);
    for f in &report.findings {
        println!("{f}");
    }
    let (errors, warnings) = (report.errors(), report.warnings());
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    if errors == 0 && warnings == 0 {
        eprintln!("gitagent validate: {} is valid", dir.display());
    } else {
        eprintln!(
            "gitagent validate: {}: {}, {}",
            dir.display(),
            plural(errors, "error"),
            plural(warnings, "warning")
        );
    }
    ExitCode::from(u8::from(errors > 0))
}
