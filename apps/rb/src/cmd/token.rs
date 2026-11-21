use rb_tokens::{Hmac256, StandardClaims};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Print a signed token.
    Issue {
        #[arg(long)]
        sub: String,
        /// Lifetime in seconds.
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
        #[arg(long, env = "RB_TOKEN_SECRET", default_value = "dev-secret-change-me", hide_env_values = true)]
        secret: String,
    },
    /// Verify a token and print its claims.
    Verify {
        token: String,
        #[arg(long, env = "RB_TOKEN_SECRET", default_value = "dev-secret-change-me", hide_env_values = true)]
        secret: String,
    },
}

pub fn run(c: Command) -> anyhow::Result<()> {
    match c {
        Command::Issue { sub, ttl, secret } => {
            let s = Hmac256::new(secret.as_bytes());
            let now = s.now_unix();
            let claims = StandardClaims {
                sub: Some(sub),
                iat: Some(now),
                exp: Some(now + ttl),
                iss: Some("rust-build".into()),
                ..Default::default()
            };
            println!("{}", s.sign(&claims)?);
        }
        Command::Verify { token, secret } => {
            let claims: StandardClaims = Hmac256::new(secret.as_bytes()).verify(&token)?;
            println!("{}", serde_json::to_string_pretty(&claims)?);
        }
    }
    Ok(())
}
