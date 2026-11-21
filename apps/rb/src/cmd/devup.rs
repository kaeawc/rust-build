//! `rb devup`: the local stack (Postgres, Valkey, OTel collector, the API) via `docker compose`.
//! Subprocesses go through [`Runner`] so tests check the exact commands without running Docker.

use anyhow::bail;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
    /// Compose file.
    #[arg(long, global = true, default_value = "docker-compose.yml")]
    file: PathBuf,
}

#[derive(clap::Subcommand, Debug)]
enum Action {
    /// Start services in the background (all when none are named) and wait for health checks.
    Up { services: Vec<String> },
    /// Stop and remove the stack (`--volumes` also deletes data).
    Down {
        #[arg(long)]
        volumes: bool,
    },
    /// List services and their state.
    Ps,
    /// Follow a service's logs.
    Logs { service: String },
    /// Restart one service.
    Restart { service: String },
    /// Print the env vars that point `rb serve` at the stack.
    Env,
}

/// Runs a program; the seam tests replace.
pub trait Runner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<i32>;
}

pub struct Os;

impl Runner for Os {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<i32> {
        let status = Command::new(program).args(args).stdin(Stdio::inherit()).status()?;
        Ok(status.code().unwrap_or(1))
    }
}

fn compose_args(file: &std::path::Path, action: &Action) -> Option<Vec<String>> {
    let mut a = vec!["compose".to_string(), "-f".to_string(), file.display().to_string()];
    match action {
        Action::Up { services } => {
            a.extend(["up", "-d", "--wait"].map(String::from));
            a.extend(services.iter().cloned());
        }
        Action::Down { volumes } => {
            a.push("down".into());
            if *volumes {
                a.push("--volumes".into());
            }
        }
        Action::Ps => a.push("ps".into()),
        Action::Logs { service } => a.extend(["logs".to_string(), "-f".to_string(), service.clone()]),
        Action::Restart { service } => a.extend(["restart".to_string(), service.clone()]),
        Action::Env => return None,
    }
    Some(a)
}

pub const STACK_ENV: &str = "DATABASE_URL=postgres://appuser:apppass@127.0.0.1:5432/appdb?sslmode=disable
VALKEY_URL=redis://127.0.0.1:6379
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318";

pub fn run_with(a: Args, runner: &dyn Runner) -> anyhow::Result<()> {
    let Some(args) = compose_args(&a.file, &a.action) else {
        println!("{STACK_ENV}");
        return Ok(());
    };
    if !a.file.exists() {
        bail!("{} not found (run from the repo root or pass --file)", a.file.display());
    }
    let code = runner.run("docker", &args)?;
    if code != 0 {
        bail!("docker {} exited with {code}", args.join(" "));
    }
    Ok(())
}

pub fn run(a: Args) -> anyhow::Result<()> {
    run_with(a, &Os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
        code: i32,
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String]) -> anyhow::Result<i32> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            self.calls.borrow_mut().push(call);
            Ok(self.code)
        }
    }

    fn args(action: Action) -> Args {
        Args { action, file: PathBuf::from("Cargo.toml") } // any existing file
    }

    #[test]
    fn builds_compose_commands_and_surfaces_failures() {
        let fake = Fake { calls: RefCell::default(), code: 0 };
        run_with(args(Action::Up { services: vec!["postgres".into()] }), &fake).expect("up");
        run_with(args(Action::Down { volumes: true }), &fake).expect("down");
        run_with(args(Action::Env), &fake).expect("env");
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2, "env runs nothing");
        assert_eq!(calls[0].join(" "), "docker compose -f Cargo.toml up -d --wait postgres");
        assert_eq!(calls[1].join(" "), "docker compose -f Cargo.toml down --volumes");

        let failing = Fake { calls: RefCell::default(), code: 3 };
        let err = run_with(args(Action::Ps), &failing).expect_err("non-zero exit fails");
        assert!(err.to_string().contains("exited with 3"));
        let missing = Args { action: Action::Ps, file: PathBuf::from("nope.yml") };
        assert!(run_with(missing, &failing).is_err());
    }
}
