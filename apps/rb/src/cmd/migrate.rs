//! `rb migrate`: inspect and apply schema migrations (golang-migrate compatible).

use crate::backend::DbArgs;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    action: Action,
    /// Use an in-memory migrator (no database), for demos and CI.
    #[arg(long, global = true)]
    fake: bool,
    #[command(flatten)]
    db: DbArgs,
}

#[derive(clap::Subcommand)]
enum Action {
    /// Current version, dirty flag, and pending migrations.
    Status,
    /// Apply every pending migration.
    Up,
    /// Apply the next migration.
    UpOne,
    /// Revert the current migration.
    Down,
    /// Set the version (or `none`) and clear the dirty flag without running SQL.
    Force { version: String },
    /// List embedded migrations.
    List,
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    if let Action::List = a.action {
        for m in rb_db::migrate::MIGRATIONS {
            println!("{:06} {}", m.version, m.name);
        }
        return Ok(());
    }
    let m = crate::backend::migrator(&a.db, a.fake)?;
    match a.action {
        Action::Status | Action::List => {}
        Action::Up => println!("applied: {:?}", m.up_all().await?),
        Action::UpOne => println!("applied: {:?}", m.up_one().await?),
        Action::Down => println!("now at: {:?}", m.down_one().await?),
        Action::Force { version } => {
            let v = if version == "none" { None } else { Some(version.parse::<i64>()?) };
            m.force(v).await?;
            println!("forced to {v:?}");
        }
    }
    let s = m.status().await?;
    println!(
        "version: {}  dirty: {}  pending: {}",
        s.current.map_or_else(|| "none".to_string(), |v| v.to_string()),
        s.dirty,
        s.pending().len()
    );
    for (v, name) in &s.migrations {
        let mark = if s.current.is_some_and(|c| *v <= c) { "applied" } else { "pending" };
        println!("  {v:06} {name:<20} {mark}");
    }
    Ok(())
}
