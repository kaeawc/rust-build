pub mod cert;
pub mod env;
pub mod get;
pub mod loadgen;
pub mod serve;
pub mod token;
pub mod tunnel;
pub mod ws;

use std::path::Path;

/// Writes `contents` to `path` via a temp file + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("file"), std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Parses `5s`, `250ms`, `2m` for clap.
pub fn duration(s: &str) -> Result<std::time::Duration, String> {
    rb_core::stats::parse_duration(s).ok_or_else(|| format!("invalid duration {s:?} (try 5s, 250ms, 2m)"))
}
