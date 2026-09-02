//! Laysi 授權伺服器.
//!
//! Two jobs in one process:
//!
//! - `POST /v1/license/check`, the single endpoint every Laysi One
//!   installation calls to activate and then to keep its lease alive.
//! - the partner console, where distributors and resellers issue licences,
//!   renew them, unbind a machine and read the month.
//!
//! The signing key lives only here. Installations carry the public half
//! compiled into the product, so a licence keeps working through a network
//! outage and cannot be forged by editing the customer's own database.
//!
//! Nothing in this server asks anybody to invent a value: the database
//! password and the signing key are generated once into a volume by the
//! compose stack, exactly as the product does it.

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use chrono::{FixedOffset, NaiveDate, Utc};
use ed25519_dalek::SigningKey;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;

mod auth;
mod check;
mod console;
mod db;
mod token;

/// Business time zone. The licence year is sold in Taiwan, so a date that
/// changes at UTC midnight would put a renewal in the wrong month twice a
/// day.
const UTC_OFFSET_HOURS: i32 = 8;

pub fn business_offset() -> FixedOffset {
    FixedOffset::east_opt(UTC_OFFSET_HOURS * 3600).expect("a fixed offset of +08:00 is valid")
}

pub struct State {
    pub pool: PgPool,
    pub signing_key: SigningKey,
}

impl State {
    pub fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&business_offset()).date_naive()
    }
}

pub type Shared = Arc<State>;

/// Read a setting from `NAME` or from the file named by `NAME_FILE`.
///
/// The file form is what lets compose generate a secret into a volume rather
/// than asking somebody to put one in an `.env`.
fn env_or_file(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        if !value.is_empty() {
            return Some(value);
        }
    }
    let path = std::env::var(format!("{name}_FILE")).ok()?;
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn signing_key() -> Result<SigningKey> {
    let text = env_or_file("LICENSE_SIGNING_KEY").context(
        "LICENSE_SIGNING_KEY or LICENSE_SIGNING_KEY_FILE must hold 32 bytes of hex; \
         compose generates it into the secrets volume",
    )?;
    token::signing_key_from_hex(&text)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Two subcommands, both run once by whoever installs this server: the
    // public key that gets compiled into the product, and the first console
    // account, which cannot be created from a console nobody can sign in to.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("public-key") => {
            let key = signing_key()?;
            println!("{}", hex::encode(key.verifying_key().to_bytes()));
            return Ok(());
        }
        // The container health check. A binary the image already has beats
        // installing curl for one line of shell.
        Some("healthcheck") => {
            use std::io::{Read, Write};
            let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".into());
            let port = bind.rsplit(':').next().unwrap_or("8090").to_string();
            let mut socket = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                .context("connecting to our own port")?;
            socket.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
            let mut response = String::new();
            socket.read_to_string(&mut response)?;
            if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
                eprintln!("healthcheck: /healthz did not return 200");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some("create-vendor") => {
            let email = args.get(1).context("usage: create-vendor <email>")?;
            let pool = db::connect(&database_url()?).await?;
            let password = create_vendor(&pool, email).await?;
            println!("帳號：{email}");
            println!("密碼：{password}");
            println!("登入後請自行更換。");
            return Ok(());
        }
        _ => {}
    }

    let pool = db::connect(&database_url()?).await?;
    let state: Shared = Arc::new(State {
        pool,
        signing_key: signing_key()?,
    });
    tracing::info!(
        public_key = %hex::encode(state.signing_key.verifying_key().to_bytes()),
        "signing key loaded"
    );

    let app = Router::new()
        // The product's side of the contract. One endpoint, no session.
        .route("/v1/license/check", post(check::check))
        .route("/healthz", get(healthz))
        // The console.
        .route("/", get(console::home))
        .route("/login", get(console::login_form).post(console::login))
        .route("/logout", post(console::logout))
        .route("/licenses", get(console::licenses).post(console::issue))
        .route("/licenses/{no}", get(console::detail))
        .route("/licenses/{no}/renew", post(console::renew))
        .route("/licenses/{no}/unbind", post(console::unbind))
        .route("/licenses/{no}/status", post(console::set_status))
        .route("/licenses/{no}/offline", post(console::offline))
        .route("/billing", get(console::billing))
        .route(
            "/partners",
            get(console::partners).post(console::create_partner),
        )
        .route("/partners/{no}", post(console::update_partner))
        .with_state(state);

    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "laysi-one-license listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

fn database_url() -> Result<String> {
    env_or_file("DATABASE_URL").context("DATABASE_URL or DATABASE_URL_FILE is required")
}

/// The first console account: the vendor's own, which every partner is then
/// created from. Re-running it on an existing address resets that password
/// rather than failing, because the alternative is a server nobody can sign
/// in to and no way back that does not involve `psql`.
async fn create_vendor(pool: &PgPool, email: &str) -> Result<String> {
    use sqlx::Row;
    let mut tx = pool.begin().await?;
    let partner_id: i32 = match sqlx::query("SELECT id FROM partner WHERE partner_no = 'P-000'")
        .fetch_optional(&mut *tx)
        .await?
    {
        Some(row) => row.get("id"),
        None => sqlx::query(
            "INSERT INTO partner (partner_no, name) VALUES ('P-000', 'Laysi') RETURNING id",
        )
        .fetch_one(&mut *tx)
        .await?
        .get("id"),
    };
    let password = console::temporary_password();
    sqlx::query(
        "INSERT INTO partner_user (partner_id, email, password_hash, name, is_vendor)
         VALUES ($1, lower($2), $3, '原廠', true)
         ON CONFLICT (email) DO UPDATE
            SET password_hash = EXCLUDED.password_hash, status = 'active'",
    )
    .bind(partner_id)
    .bind(email)
    .bind(auth::hash_password(&password)?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(password)
}

async fn healthz(
    axum::extract::State(s): axum::extract::State<Shared>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ok = sqlx::query("SELECT 1").fetch_one(&s.pool).await.is_ok();
    let code = if ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        axum::Json(serde_json::json!({ "status": if ok { "ok" } else { "degraded" } })),
    )
        .into_response()
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
