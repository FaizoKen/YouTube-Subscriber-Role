use std::sync::Arc;

use axum::routing::{delete, get, post};
use axum::Router;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

mod config;
mod db;
mod error;
mod models;
mod routes;
mod schema;
mod services;
mod tasks;

use services::quota::QuotaGovernor;
use services::rolelogic::RoleLogicClient;
use services::sync::{ConfigSyncEvent, PlayerSyncEvent};
use services::youtube::YouTubeClient;

pub struct AppState {
    pub pool: PgPool,
    pub config: config::AppConfig,
    pub player_sync_tx: mpsc::Sender<PlayerSyncEvent>,
    pub config_sync_tx: mpsc::Sender<ConfigSyncEvent>,
    pub youtube_client: YouTubeClient,
    /// Central daily-quota budget + pacing for every YouTube Data API call.
    pub quota: Arc<QuotaGovernor>,
    pub rl_client: RoleLogicClient,
    pub http: reqwest::Client,
    pub verify_html: bytes::Bytes,
    pub subscribers_html: bytes::Bytes,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "youtube_sub_role=info,tower_http=info".into()),
        )
        .init();

    let app_config = config::AppConfig::from_env();
    let listen_addr = app_config.listen_addr.clone();

    let pool = db::create_pool(&app_config.database_url).await;
    db::run_migrations(&pool).await;
    tracing::info!("Database connected and migrations applied");

    let refresh_workers = app_config.refresh_workers;

    // Central quota governor — loads today's spend from the durable ledger so a
    // restart resumes accounting instead of resetting to zero and over-spending.
    let quota = QuotaGovernor::new(
        pool.clone(),
        app_config.youtube_quota_per_day,
        app_config.quota_interactive_reserve,
        app_config.quota_safety_fraction,
    )
    .await;

    let (player_sync_tx, player_sync_rx) = mpsc::channel::<PlayerSyncEvent>(2048);
    let (config_sync_tx, config_sync_rx) = mpsc::channel::<ConfigSyncEvent>(64);

    let youtube_client = YouTubeClient::new();
    let rl_client = RoleLogicClient::new();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to build OAuth HTTP client");
    let verify_html = bytes::Bytes::from(routes::verification::render_verify_page(
        &app_config.base_url,
    ));
    let subscribers_html = bytes::Bytes::from(routes::subscribers::render_subscribers_page(
        &app_config.base_url,
    ));

    let state = Arc::new(AppState {
        pool,
        config: app_config,
        player_sync_tx,
        config_sync_tx,
        youtube_client,
        quota: Arc::clone(&quota),
        rl_client,
        http,
        verify_html,
        subscribers_html,
    });

    // Persist the quota ledger on an interval.
    tokio::spawn(Arc::clone(&quota).run_flusher());

    // Spawn background refresh workers, partitioned by hashtext(discord_id) % N
    // so they never double-process. They share the one quota governor's budget.
    for worker_id in 0..refresh_workers {
        tokio::spawn(tasks::refresh_worker::run(
            Arc::clone(&state),
            worker_id,
            refresh_workers,
        ));
    }
    tokio::spawn(tasks::player_sync_worker::run(
        player_sync_rx,
        Arc::clone(&state),
    ));
    tokio::spawn(tasks::config_sync_worker::run(
        config_sync_rx,
        Arc::clone(&state),
    ));
    tokio::spawn(tasks::cleanup_expired(Arc::clone(&state)));

    let app = Router::new()
        .nest(
            "/youtube-subscriber-role",
            Router::new()
                // Plugin endpoints (called by RoleLogic)
                .route("/register", post(routes::plugin::register))
                .route("/config", get(routes::plugin::get_config))
                .route("/config", post(routes::plugin::post_config))
                .route("/config", delete(routes::plugin::delete_config))
                // Subscribers list (user-facing, auth via cookie)
                .route(
                    "/subscribers/{guild_id}",
                    get(routes::subscribers::subscribers_page),
                )
                .route(
                    "/subscribers/{guild_id}/data",
                    get(routes::subscribers::subscribers_data),
                )
                // Admin role-config (iframe UI mode)
                .route(
                    "/admin/{guild_id}/role/{role_id}",
                    get(routes::admin::role_config_page),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/data",
                    get(routes::admin::role_config_data),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/save",
                    post(routes::admin::role_config_save),
                )
                .route(
                    "/admin/{guild_id}/role/{role_id}/preview",
                    get(routes::admin::role_config_preview)
                        .post(routes::admin::role_config_preview_edit),
                )
                .route(
                    "/admin/{guild_id}/view-permission",
                    post(routes::admin::set_view_permission),
                )
                // Verification endpoints (user-facing)
                .route("/verify", get(routes::verification::verify_page))
                .route(
                    "/verify/channels",
                    get(routes::verification::verify_channels),
                )
                .route("/verify/login", get(routes::verification::login))
                .route("/verify/youtube", get(routes::verification::youtube_login))
                .route(
                    "/verify/youtube/callback",
                    get(routes::verification::youtube_callback),
                )
                .route("/verify/status", get(routes::verification::status))
                .route("/verify/refresh", post(routes::verification::refresh))
                .route("/verify/unlink", post(routes::verification::unlink))
                .route("/verify/logout", post(routes::verification::logout))
                // Health & static
                .route("/favicon.ico", get(routes::health::favicon))
                .route("/dweeb/status", get(routes::dweeb::status))
                .route("/health", get(routes::health::health)),
        )
        .layer(axum::middleware::from_fn(
            services::security_headers::baseline,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    tracing::info!("Server starting on {listen_addr}");

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .expect("Failed to bind listener");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutdown signal received, draining connections...");
        })
        .await
        .expect("Server error");

    tracing::info!("Server stopped");
}
