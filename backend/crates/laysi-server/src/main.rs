//! The Laysi One server: REST and MCP over one operation registry.
//!
//! The startup order is deliberate:
//!
//! 1. Brand integrity verification, first of all. Failing it refuses to boot
//!    without even connecting to the database.
//! 2. Migrations.
//! 3. Load the rulesets out of `reg_ruleset`.
//! 4. Start serving.
//!
//! Step 1 being strict is a product decision. Its cost is that editing a
//! brand file stops the server booting; the remedy is
//! `laysi-brandhash --update`, and the error message prints that command.
//!
//! `/v1/auth/*` is the one exception to "do not hand-write axum routes".
//! Ordinary operations must pass the role permission check in the registry's
//! dispatch, but signing in is how a trusted identity is obtained in the
//! first place; making login an operation would require being logged in to
//! log in. `/v1/auth/bootstrap` is the same argument one step earlier: it
//! creates the identity there is nothing to check against yet, and is gated
//! by the installation having no accounts instead. The auth endpoints are
//! therefore listed explicitly here, and everything else still goes through
//! the registry.

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use laysi_ops::{OpCtx, Registry};
use laysi_store::{ObjectStore, Store};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

mod auth;
mod mcp;
mod oidc;
mod ratelimit;
mod setup;

/// MCP protocol version. Tied to the shape of `tools/list`; check both when
/// bumping it.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Largest sign-in body accepted. A login, a password change and the bootstrap
/// call are all a few hundred bytes; anything larger is not a client of this
/// endpoint, and these are the only routes reachable without a session.
const AUTH_BODY_LIMIT: usize = 64 * 1024;

/// Largest operation body accepted, on both `/v1/ops/{name}` and `/mcp`.
///
/// axum applies its own 2 MB default to every `Json` and `Bytes` extractor,
/// which is below what `upload_document` needs: a file is base64 inside the
/// operation input, so a 25 MB one (`storage::MAX_OBJECT_BYTES`) arrives as
/// roughly 33.4 MB of body. The request was rejected by the extractor before
/// the operation's own size check could report anything, and the screen
/// offers files up to that same 25 MB. The headroom above 4/3 covers the JSON
/// envelope and the file name.
const OPS_BODY_LIMIT: usize = 40 * 1024 * 1024;

// base64 costs four characters per three bytes, so the ceiling has to clear
// that much of a maximum file before the JSON envelope is even counted.
const _: () = assert!(OPS_BODY_LIMIT > laysi_store::storage::MAX_OBJECT_BYTES / 3 * 4);
const _: () = assert!(AUTH_BODY_LIMIT < OPS_BODY_LIMIT);

/// Which routes this process mounts, decided by `LAYSI_SERVE`.
///
/// A separate `mcp` role is the main reason that container exists: the
/// process exposed to AI connectors has no REST, no session cookie and no
/// login endpoint to attack. The reduction in attack surface is structural
/// (fewer routes mounted), not a set of paths a reverse proxy blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeRole {
    /// Every route: `/healthz`, `/v1/*`, `/mcp`. A simple single-process
    /// deployment.
    Full,
    /// `/healthz` and `/v1/*`, without `/mcp`. The compose `api` service.
    App,
    /// `/healthz` and `/mcp`, without any `/v1/*`. The compose `mcp` service.
    Mcp,
}

impl ServeRole {
    /// An unrecognised value must fail at startup rather than silently
    /// meaning `full`, which would leave a container believed to be MCP-only
    /// serving the whole REST API.
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("LAYSI_SERVE").ok().as_deref() {
            None | Some("full") => Ok(Self::Full),
            Some("app") => Ok(Self::App),
            Some("mcp") => Ok(Self::Mcp),
            Some(other) => Err(anyhow::anyhow!(
                "LAYSI_SERVE 的值「{other}」無法識別。合法值：full（預設，全部路由）、\
                 app（/healthz 與 /v1/*，不含 /mcp）、mcp（/healthz 與 /mcp，不含任何 /v1/*）。"
            )),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::App => "app",
            Self::Mcp => "mcp",
        }
    }

    fn serves_rest(&self) -> bool {
        matches!(self, Self::Full | Self::App)
    }

    fn serves_mcp(&self) -> bool {
        matches!(self, Self::Full | Self::Mcp)
    }
}

pub struct AppState {
    pub store: Store,
    pub registry: Registry,
    pub brand: laysi_brand::Report,
    pub auth: auth::AuthConfig,
    /// Whether the browser may create the installation's first account.
    pub setup: setup::SetupConfig,
    /// External sign-in, rebuilt from `oidc_setting` whenever that row
    /// changes. Not built once at startup: the settings are edited on screen
    /// and a restart is not something their owner should have to arrange.
    pub oidc: tokio::sync::RwLock<oidc::OidcCache>,
    /// RustFS client. `None` when the configuration is incomplete, which
    /// `/healthz` reports as such.
    pub objects: Option<ObjectStore>,
    pub rustfs_endpoint: String,
    /// Per-token rate limit for `/mcp`, counted in process, with no redis
    /// dependency.
    pub mcp_rate_limiter: ratelimit::RateLimiter,
    /// Attempt limit for `/v1/auth/bootstrap`, which has no token to key on
    /// and so shares one bucket.
    pub setup_rate_limiter: ratelimit::RateLimiter,
    /// Attempt limit for `/v1/auth/login`, bucketed by login name.
    pub login_rate_limiter: ratelimit::RateLimiter,
    /// Allowed `Origin` values for `/mcp`, guarding against DNS rebinding. A
    /// request that carries an Origin must match this list; one that does not
    /// (as most AI connectors do not) is allowed. Empty by default.
    pub mcp_allowed_origins: Vec<String>,
}

pub type Shared = Arc<AppState>;

/// Container health check: call our own `/healthz` and treat 200 as healthy.
///
/// Implemented as a flag on the same binary rather than a separate tool: the
/// runtime image has no curl, and another tool is another thing that can
/// drift from the server's behaviour.
fn healthcheck() -> ! {
    use std::io::{Read, Write};

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let port = addr.rsplit(':').next().unwrap_or("8080");

    let probe = || -> std::io::Result<bool> {
        let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(4)))?;
        s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut buf = String::new();
        s.read_to_string(&mut buf)?;
        Ok(buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200"))
    };

    match probe() {
        Ok(true) => std::process::exit(0),
        Ok(false) => {
            eprintln!("healthcheck：/healthz 沒有回 200");
            std::process::exit(1)
        }
        Err(e) => {
            eprintln!("healthcheck：無法連線 127.0.0.1:{port}：{e}");
            std::process::exit(1)
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The health check short-circuits ahead of brand verification and the
    // database connection; it only asks whether the service is alive.
    if std::env::args().any(|a| a == "--healthcheck") {
        healthcheck();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    // 1. Brand integrity: no boot without it.
    let brand = match laysi_brand::verify() {
        Ok(r) => {
            tracing::info!(files = r.files, fingerprint = %r.fingerprint, "品牌完整性驗證通過");
            r
        }
        Err(e) => {
            eprintln!("\n╔══════════════════════════════════════════════════════════════╗");
            eprintln!("║  Laysi One 拒絕啟動：品牌完整性驗證失敗                       ║");
            eprintln!("╚══════════════════════════════════════════════════════════════╝\n");
            eprintln!("{e}\n");
            // EX_CONFIG: this is a build or configuration problem, not a
            // runtime error.
            std::process::exit(78);
        }
    };

    let serve_role = ServeRole::from_env()?;
    tracing::info!(role = serve_role.label(), "LAYSI_SERVE 決定要掛哪些路由");

    let database_url = laysi_store::env_or_file("DATABASE_URL")
        .unwrap_or_else(|| "postgres://postgres:postgres@localhost:5432/laysi".into());
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let rustfs_endpoint =
        std::env::var("RUSTFS_ENDPOINT").unwrap_or_else(|_| "http://rustfs:9000".into());
    let auth = auth::AuthConfig::from_env()?;
    let setup = setup::SetupConfig::from_env()?;
    let cors_origin =
        std::env::var("CORS_ALLOWED_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".into());
    let cors_origin = HeaderValue::from_str(&cors_origin)
        .map_err(|e| anyhow::anyhow!("CORS_ALLOWED_ORIGIN 不是有效的 HTTP origin：{e}"))?;
    // MCP Origin allow list, comma separated, empty by default. That accepts
    // only clients that send no Origin (normal AI connectors do not send it),
    // while a browser sending one must appear in the list.
    let mcp_allowed_origins: Vec<String> = std::env::var("MCP_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // 2. Database
    let store = Store::connect(&database_url, 8).await?;
    store.migrate().await?;
    // Before anything can date a document. `today()` reads it, and the
    // constant it falls back to is the one the CHECK constraints hold.
    let offset = laysi_ops::load_business_offset(&store).await;
    // Session timeouts and the MCP allowance are settings on a screen, read on
    // paths that have no Store at hand. update_company_setting republishes
    // them, so a change needs no restart.
    laysi_ops::load_policy_settings(&store).await;
    tracing::info!(offset = %laysi_domain::format_business_offset(offset), "記帳時區");
    tracing::info!(version = ?store.migration_version().await?, "migrations 完成");

    // 3. Rulesets
    //
    // `reg_ruleset` is the authority; the embedded factory set only fills
    // rows the table does not have. Nothing may compute before this runs.
    let n = store.reload_regulations().await?;
    tracing::info!(rulesets = n, "法規版本已載入");

    // The work calendar, once, on the first boot that finds it empty and can
    // reach the open-data platform. Spawned rather than awaited: a new
    // installation should not wait on a government website to start, and one
    // with no way out to the internet should not fail to.
    {
        let store = store.clone();
        let today = laysi_domain::business_today(offset);
        tokio::spawn(async move { laysi_ops::ops::calendar::seed(&store, today).await });
    }

    // 4. Object storage
    //
    // Create the bucket if it does not exist. RustFS being down does not
    // refuse the boot: accounting does not depend on object storage, only the
    // document module does. A failed connection logs a warning and both
    // `/healthz` and the document screen report it, rather than stopping the
    // whole system for the sake of attachments.
    let objects = match ObjectStore::from_env() {
        Ok(o) => {
            match o.ensure_bucket().await {
                Ok(created) => tracing::info!(
                    bucket = o.bucket(),
                    endpoint = o.endpoint(),
                    created,
                    "RustFS 物件儲存就緒"
                ),
                Err(e) => tracing::warn!(error = %e, "RustFS 連得上設定但桶子備妥失敗"),
            }
            Some(o)
        }
        Err(e) => {
            tracing::warn!(error = %e, "RustFS 未設定，文件模組停用，其餘功能不受影響");
            None
        }
    };

    let registry = Registry::new();
    tracing::info!(
        operations = registry.len(),
        "操作註冊表就緒（REST 與 MCP 共用）"
    );

    tracing::info!(
        limit_per_minute = laysi_ops::mcp_rate_limit_per_minute(),
        allowed_origins = mcp_allowed_origins.len(),
        "MCP 速率限制與 Origin 白名單設定完成"
    );

    let state: Shared = Arc::new(AppState {
        store,
        registry,
        brand,
        auth,
        setup,
        oidc: tokio::sync::RwLock::new(oidc::OidcCache::default()),
        objects,
        rustfs_endpoint,
        mcp_rate_limiter: ratelimit::RateLimiter::from_setting(
            laysi_ops::mcp_rate_limit_per_minute,
        ),
        setup_rate_limiter: ratelimit::RateLimiter::new(setup::ATTEMPTS_PER_MINUTE),
        login_rate_limiter: ratelimit::RateLimiter::new(auth::LOGIN_ATTEMPTS_PER_MINUTE),
        mcp_allowed_origins,
    });

    // Building the client now turns a broken configuration into a line in the
    // log at boot, rather than into a sign-in button that fails for the first
    // person who presses it.
    if serve_role.serves_rest() {
        let enabled = oidc::current(&state).await.is_some();
        tracing::info!(enabled, "外部登入設定已讀取");
    }

    // `/healthz` is mounted in all three roles: the compose healthcheck relies
    // on it whether the process is full, REST-only or MCP-only.
    let mut app = Router::new().route("/healthz", get(healthz));

    if serve_role.serves_rest() {
        app = app
            .merge(
                Router::new()
                    .route("/v1/auth/login", post(auth::login))
                    .route("/v1/auth/logout", post(auth::logout))
                    .route("/v1/auth/me", get(auth::me))
                    .route("/v1/auth/change-password", post(auth::change_password))
                    .route("/v1/auth/bootstrap", post(setup::bootstrap))
                    .route("/v1/auth/oauth/start", get(oidc::start))
                    .route("/v1/auth/oauth/callback", get(oidc::callback))
                    .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)),
            )
            .route("/v1/meta", get(meta))
            .route("/v1/brand/mark.svg", get(brand_mark))
            .route("/v1/brand/monogram.svg", get(brand_monogram))
            .route("/v1/ops", get(list_ops))
            .route(
                "/v1/ops/{name}",
                post(call_op).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)),
            );
    }

    // In the `mcp` role nothing other than /mcp may be mounted. The reason
    // that container exists is that the externally exposed process has no
    // REST at all, rather than a reverse proxy blocking it.
    if serve_role.serves_mcp() {
        app = app.route(
            "/mcp",
            post(mcp::handle).layer(DefaultBodyLimit::max(OPS_BODY_LIMIT)),
        );
    }

    let app = app
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(cors_origin)
                .allow_credentials(true)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "Laysi One 開始服務");
    axum::serve(listener, app).await?;
    Ok(())
}

// Identity

/// The current business date.
///
/// Not the server's UTC date. The container runs on UTC and the business runs
/// on UTC+8, so between midnight and 08:00 local the two disagree and every
/// document created in that window would be dated the day before.
///
/// `LAYSI_TODAY` overrides the date for demonstration installs. It does not
/// move the clock: a clock-in records the real instant it happened, and the
/// database checks that its date and its timestamp agree.
pub fn today() -> chrono::NaiveDate {
    std::env::var("LAYSI_TODAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| laysi_domain::business_today(laysi_ops::business_offset()))
}

pub fn ctx_from(
    state: &Shared,
    identity: &auth::VerifiedIdentity,
    prompt: Option<String>,
    channel: laysi_ops::Channel,
) -> OpCtx {
    OpCtx {
        store: state.store.clone(),
        objects: state.objects.clone(),
        actor: identity.actor.clone(),
        actor_role: identity.actor_role.clone(),
        username: identity.username.clone(),
        employee_id: identity.employee_id,
        prompt,
        channel,
        today: today(),
        offset: laysi_ops::business_offset(),
    }
}

// Endpoints

/// Liveness for anyone, detail for an administrator.
///
/// This route is reachable without signing in, and has to be: the compose
/// healthcheck runs before an account exists and the reverse proxy in front
/// of the web container passes it through. So the anonymous answer is only
/// whether each part is up. The storage endpoint and bucket, the migration
/// version and the brand fingerprint describe the deployment's insides, and
/// go out only to a signed-in administrator -- which is who the security
/// screen shows them to anyway.
async fn healthz(State(s): State<Shared>, headers: HeaderMap) -> Response {
    let migration = s.store.migration_version().await.ok().flatten();
    let db_ok = migration.is_some();
    let rustfs = probe_rustfs(&s).await;
    let rustfs_ok = rustfs.get("ok").cloned().unwrap_or(Value::Bool(false));

    let mut body = json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "brand": { "ok": true },
        "db": { "ok": db_ok },
        "rustfs": { "ok": rustfs_ok },
    });

    if admin_session(&s, &headers).await {
        body["brand"] = json!({
            "ok": true,
            "files": s.brand.files,
            "signed": s.brand.signed,
            "fingerprint": s.brand.fingerprint,
        });
        body["db"] = json!({ "ok": db_ok, "migration": migration });
        body["rustfs"] = rustfs;
        body["operations"] = json!(s.registry.len());
    }

    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body)).into_response()
}

/// Whether the request carries a session cookie belonging to an administrator.
/// Anything else -- no cookie, an expired one, another role -- is a no, and
/// never an error: this only decides how much `/healthz` says.
async fn admin_session(s: &Shared, headers: &HeaderMap) -> bool {
    match auth::authenticate_session(s, headers).await {
        Ok(verified) => verified.identity().actor_role == laysi_ops::ROLE_ADMIN,
        Err(_) => false,
    }
}

/// RustFS probe: actually call S3 to list buckets.
///
/// A TCP connectivity check was enough before anything used RustFS. Now that
/// the document module stores files there, "the port is open" is no longer a
/// useful answer: wrong credentials, a deleted bucket or a full disk all
/// still accept TCP while the health check says ok, until someone uploads.
///
/// So it lists buckets instead. Only a successful listing counts as healthy,
/// and it reports whether our bucket is present while it is there.
async fn probe_rustfs(s: &Shared) -> Value {
    let Some(objects) = &s.objects else {
        return json!({
            "ok": false,
            "endpoint": s.rustfs_endpoint,
            "configured": false,
            "note": "RUSTFS_ENDPOINT／ACCESS_KEY／SECRET_KEY 沒設齊，文件模組停用",
        });
    };

    let h = objects.health().await;
    json!({
        // A missing bucket is unhealthy too: uploads will fail, and saying ok
        // only defers the problem.
        "ok": h.reachable && h.bucket_exists,
        "endpoint": h.endpoint,
        "configured": true,
        "bucket": h.bucket,
        "bucket_exists": h.bucket_exists,
        "buckets": h.bucket_count,
        "error": h.error,
        "probe": "s3:ListBuckets",
    })
}

async fn meta(State(s): State<Shared>, headers: HeaderMap) -> Response {
    let verified = match auth::authenticate_session(&s, &headers).await {
        Ok(verified) => verified,
        Err(error) => return error.into_response(),
    };
    let identity = verified.identity();
    let settings = match s.store.company_settings().await {
        Ok(settings) => settings,
        Err(error) => {
            tracing::error!(%error, "讀取 /v1/meta 的公司設定失敗");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "ok": false,
                    "error": {
                        "code": "store_error",
                        "message": "讀取公司設定失敗",
                    },
                })),
            )
                .into_response();
        }
    };
    let setting_value = |code: &str| {
        settings
            .iter()
            .find(|setting| setting.code == code)
            .map(|setting| setting.value.as_str())
            .unwrap_or("")
    };
    let company = setting_value("company_name");
    Json(json!({
        "company": company,
        "tax_id": setting_value("tax_id"),
        // The company name is still the empty string a fresh install ships
        // with, so first-run setup has not been completed.
        "setup_required": company_setup_required(company),
        "period": today().format("%Y / %m").to_string(),
        // The screen turns instants into wall-clock times with this, so it
        // must be the same one the server dated `period` with.
        "utc_offset": laysi_domain::format_business_offset(laysi_ops::business_offset()),
        "period_state": "未結帳",
        "actor": identity.actor,
        "actor_role": identity.actor_role,
        "employee_id": identity.employee_id,
        "oauth": oidc::describe(&s).await,
        "version": env!("CARGO_PKG_VERSION"),
        "licence": "BSL",
        "brand_build": s.brand.fingerprint,
    }))
    .into_response()
}

fn company_setup_required(company_name: &str) -> bool {
    company_name.trim().is_empty()
}

#[derive(Deserialize)]
struct MarkQuery {
    h: Option<f64>,
}

fn svg(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            // The mark changes with the build and the fingerprint is in the
            // body, so a short cache is enough.
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
        .into_response()
}

async fn brand_mark(State(s): State<Shared>, Query(q): Query<MarkQuery>) -> Response {
    svg(laysi_brand::wordmark(&laysi_brand::MarkOptions {
        height: q.h.unwrap_or(28.0).clamp(8.0, 512.0),
        color: "currentColor".into(),
        build: s.brand.fingerprint.clone(),
    }))
}

async fn brand_monogram(State(s): State<Shared>, Query(q): Query<MarkQuery>) -> Response {
    svg(laysi_brand::monogram(&laysi_brand::MarkOptions {
        height: q.h.unwrap_or(64.0).clamp(8.0, 512.0),
        color: "currentColor".into(),
        build: s.brand.fingerprint.clone(),
    }))
}

/// Public listing of the registry, used by the MCP screen to render the tool
/// switches.
async fn list_ops(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(error) = auth::authenticate_session(&s, &headers).await {
        return error.into_response();
    }
    let grants = s.store.tool_grants().await.unwrap_or_default();
    let ops: Vec<Value> = s
        .registry
        .iter()
        .map(|op| {
            json!({
                "name": op.name(),
                "risk": op.risk(),
                "risk_label": op.risk().label_zh(),
                "summary": op.summary(),
                "enabled": grants.iter().find(|(n, _)| n == op.name()).map(|(_, e)| *e).unwrap_or(false),
                "input_schema": op.input_schema(),
            })
        })
        .collect();
    Json(json!({ "operations": ops })).into_response()
}

#[derive(Deserialize)]
struct CallBody {
    #[serde(default)]
    input: Value,
    /// What the user actually said to the AI, recorded as intent in the audit.
    #[serde(default)]
    prompt: Option<String>,
}

async fn call_op(
    State(s): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Option<Json<CallBody>>,
) -> Response {
    let verified = match auth::authenticate_session(&s, &headers).await {
        Ok(verified) => verified,
        Err(error) => return error.into_response(),
    };
    let Json(body) = body.unwrap_or(Json(CallBody {
        input: Value::Null,
        prompt: None,
    }));
    // Operations from the screen use the Ui channel and are not subject to the
    // AI tool grants.
    let ctx = ctx_from(
        &s,
        &verified.identity(),
        body.prompt,
        laysi_ops::Channel::Ui,
    );

    match s.registry.dispatch(&ctx, &name, body.input).await {
        Ok(data) => {
            // Discovery belongs to this layer, so the settings screen would
            // otherwise show the previous answer until something else
            // happened to ask. Rebuilding here means the next read of the
            // settings already says whether they work.
            if name == "update_oidc_setting" {
                oidc::current(&s).await;
            }
            Json(json!({ "ok": true, "data": data })).into_response()
        }
        Err(e) => {
            let code = match e {
                laysi_ops::OpError::UnknownTool(_) => StatusCode::NOT_FOUND,
                laysi_ops::OpError::ToolDisabled { .. }
                | laysi_ops::OpError::NeedsBoss { .. }
                | laysi_ops::OpError::NotPermitted { .. } => StatusCode::FORBIDDEN,
                laysi_ops::OpError::BadInput(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (
                code,
                Json(
                    json!({ "ok": false, "error": { "code": e.code(), "message": e.to_string() }}),
                ),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::company_setup_required;

    #[test]
    fn an_empty_company_name_requires_initial_setup() {
        assert!(company_setup_required(""));
        assert!(company_setup_required("  "));
        assert!(!company_setup_required("測試股份有限公司"));
    }
}
