use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// DNS names / IPs the certificate is valid for (comma-separated).
    #[arg(long, value_delimiter = ',', default_value = "localhost,127.0.0.1,::1")]
    hosts: Vec<String>,
    /// Directory to write `cert.pem` and `key.pem` into.
    #[arg(long, default_value = "certs")]
    out: PathBuf,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let ss = rb_tls::self_signed(&a.hosts)?;
    let cert = a.out.join("cert.pem");
    let key = a.out.join("key.pem");
    super::write_atomic(&cert, ss.cert_pem.as_bytes())?;
    super::write_atomic(&key, ss.key_pem.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("wrote {} and {} for {}", cert.display(), key.display(), a.hosts.join(", "));
    Ok(())
}
