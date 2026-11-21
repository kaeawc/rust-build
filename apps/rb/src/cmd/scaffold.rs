//! `rb scaffold`: generate boilerplate that follows this repo's conventions.

use anyhow::{Context as _, bail};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    kind: Kind,
    /// Repository root.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    /// Print what would be written without writing.
    #[arg(long, global = true)]
    dry_run: bool,
}

#[derive(clap::Subcommand)]
enum Kind {
    /// A library crate under crates/ (workspace-inherited manifest, unit test).
    Crate { name: String },
    /// An `rb` subcommand under apps/rb/src/cmd/.
    Command { name: String },
    /// An axum handler module under crates/rb-server/src/handlers/.
    Handler { name: String },
}

fn valid(name: &str, allow_dash: bool) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || (allow_dash && c == '-'))
}

/// `(path, contents)` pairs to write, plus follow-up instructions.
fn plan(kind: &Kind) -> anyhow::Result<(Vec<(PathBuf, String)>, String)> {
    Ok(match kind {
        Kind::Crate { name } => {
            if !valid(name, true) {
                bail!("crate names are lowercase letters, digits, '-' or '_', starting with a letter");
            }
            let ident = name.replace('-', "_");
            let manifest = format!(
                "[package]\nname = \"{name}\"\nversion.workspace = true\nedition.workspace = true\nrust-version.workspace = true\nlicense.workspace = true\npublish.workspace = true\n\n[lib]\ndoctest = false\n\n[lints]\nworkspace = true\n\n[dependencies]\n"
            );
            let lib = format!(
                "//! {name}.\n\npub fn hello() -> &'static str {{\n    \"hello from {name}\"\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn says_hello() {{\n        assert_eq!(super::hello(), \"hello from {name}\");\n    }}\n}}\n"
            );
            (
                vec![
                    (PathBuf::from(format!("crates/{name}/Cargo.toml")), manifest),
                    (PathBuf::from(format!("crates/{name}/src/lib.rs")), lib),
                ],
                format!(
                    "Add `{name} = {{ path = \"crates/{name}\" }}` to [workspace.dependencies] to use it from other crates (`{ident}::hello()`)."
                ),
            )
        }
        Kind::Command { name } => {
            if !valid(name, false) {
                bail!("command names are snake_case");
            }
            let module = format!(
                "//! `rb {cmd}`.\n\n#[derive(clap::Args)]\npub struct Args {{\n    /// Who to greet.\n    #[arg(long, default_value = \"world\")]\n    name: String,\n}}\n\npub async fn run(a: Args) -> anyhow::Result<()> {{\n    println!(\"hello, {{}}\", a.name);\n    Ok(())\n}}\n",
                cmd = name.replace('_', "-")
            );
            let variant: String = name.split('_').map(|p| p[..1].to_uppercase() + &p[1..]).collect();
            (
                vec![(PathBuf::from(format!("apps/rb/src/cmd/{name}.rs")), module)],
                format!(
                    "Wire it up: add `pub mod {name};` to apps/rb/src/cmd/mod.rs, a `{variant}(cmd::{name}::Args)` variant to `Command` in main.rs, and `Command::{variant}(a) => cmd::{name}::run(a).await` to the match."
                ),
            )
        }
        Kind::Handler { name } => {
            if !valid(name, false) {
                bail!("handler names are snake_case");
            }
            let module = format!(
                "//! `GET /api/{route}`.\n\nuse axum::Json;\nuse serde_json::{{Value, json}};\n\npub async fn get() -> Json<Value> {{\n    Json(json!({{ \"{name}\": \"ok\" }}))\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[tokio::test]\n    async fn returns_ok() {{\n        let axum::Json(v) = super::get().await;\n        assert_eq!(v[\"{name}\"], \"ok\");\n    }}\n}}\n",
                route = name.replace('_', "-")
            );
            (
                vec![(PathBuf::from(format!("crates/rb-server/src/handlers/{name}.rs")), module)],
                format!(
                    "Wire it up: add `mod handlers {{ pub mod {name}; }}` (or `pub mod {name};` in handlers/mod.rs) to rb-server's lib.rs and `.route(\"/{}\", get(handlers::{name}::get))` to the `/api` router in api.rs.",
                    name.replace('_', "-")
                ),
            )
        }
    })
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let (files, next) = plan(&a.kind)?;
    for (rel, _) in &files {
        let path = a.root.join(rel);
        if path.exists() {
            bail!("{} already exists; not overwriting", path.display());
        }
    }
    for (rel, contents) in &files {
        let path = a.root.join(rel);
        if a.dry_run {
            println!("would write {}:\n{contents}", path.display());
            continue;
        }
        write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    println!("{next}");
    Ok(())
}

fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    super::write_atomic(path, contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names_and_plans_files() {
        assert!(valid("my_thing", false) && valid("rb-thing", true));
        assert!(!valid("Bad", false) && !valid("1x", false) && !valid("a-b", false) && !valid("", true));
        let (files, next) = plan(&Kind::Crate { name: "rb-widgets".into() }).expect("plan");
        assert_eq!(files[0].0, PathBuf::from("crates/rb-widgets/Cargo.toml"));
        assert!(files[0].1.contains("version.workspace = true") && files[0].1.contains("doctest = false"));
        assert!(next.contains("rb_widgets::hello()"));
        let (_, next) = plan(&Kind::Command { name: "say_hi".into() }).expect("plan");
        assert!(next.contains("SayHi(cmd::say_hi::Args)"));
        assert!(plan(&Kind::Handler { name: "Nope".into() }).is_err());
    }

    #[test]
    fn writes_into_root_and_refuses_overwrites() {
        let root = std::env::temp_dir().join(format!("rb-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let args = || Args { kind: Kind::Crate { name: "demo".into() }, root: root.clone(), dry_run: false };
        run(args()).expect("first write");
        assert!(root.join("crates/demo/src/lib.rs").exists());
        assert!(run(args()).is_err(), "second write refuses");
        let _ = std::fs::remove_dir_all(&root);
    }
}
