#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use chrono::{SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "LastCommit";
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";
const DEFAULT_GITHUB_ORG: &str = "wavey-ai";
const DEFAULT_INACTIVE_DAYS: u64 = 180;
const MAX_REPO_PAGES: u32 = 20;
const STATUS_KV_BINDING: &str = "LASTCOMMIT_STATUS";
const STATUS_KV_KEY: &str = "lastcommit:deadman-status";

type RunResult<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy)]
enum ExecutionMode {
    Execute,
}

impl ExecutionMode {
    fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Execute => "execute",
        }
    }
}

#[derive(Debug, Clone)]
enum RepoSelection {
    Explicit(Vec<String>),
    AllPrivate,
    Empty,
}

impl RepoSelection {
    fn source(&self) -> &'static str {
        match self {
            RepoSelection::Explicit(_) => "explicit",
            RepoSelection::AllPrivate => "allPrivate",
            RepoSelection::Empty => "empty",
        }
    }

    fn configured(&self) -> bool {
        !matches!(self, RepoSelection::Empty)
    }
}

#[derive(Debug, Clone)]
struct LastCommitConfig {
    org: String,
    inactive_days: u64,
    trusted_logins: Vec<String>,
    watch_repos: RepoSelection,
    release_repos: RepoSelection,
    armed: bool,
    github_api_base: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthReport {
    service: &'static str,
    ok: bool,
    org: String,
    armed: bool,
    inactive_days: u64,
    trusted_logins_configured: bool,
    trusted_login_count: usize,
    watch_repo_source: &'static str,
    release_repo_source: &'static str,
    endpoints: Vec<&'static str>,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeadmanReport {
    service: &'static str,
    trigger: String,
    mode: &'static str,
    org: String,
    checked_at: String,
    inactive_days: u64,
    threshold_at: String,
    armed: bool,
    status: DeadmanStatus,
    trusted_logins: Vec<String>,
    watch_repo_source: &'static str,
    watched_repos: Vec<String>,
    release_repo_source: &'static str,
    heartbeat: Option<Heartbeat>,
    planned_actions: Vec<RepoAction>,
    executed_actions: Vec<RepoAction>,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
enum DeadmanStatus {
    Alive,
    DeadDryRun,
    DeadExecuted,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Heartbeat {
    login: String,
    repo: String,
    match_kind: String,
    since: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoAction {
    repo: String,
    action: &'static str,
    executed: bool,
    ok: bool,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrafficLightStatus {
    service: &'static str,
    light: &'static str,
    status: &'static str,
    message: String,
    checked_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedDeadmanReport {
    #[serde(default)]
    checked_at: String,
    status: Option<CachedDeadmanStatus>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CachedDeadmanStatus {
    Alive,
    DeadDryRun,
    DeadExecuted,
    Blocked,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRepo {
    name: String,
    #[serde(default)]
    private: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCommit {
    author: Option<GithubUser>,
    committer: Option<GithubUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubUser {
    login: String,
}

trait GithubApi {
    async fn list_private_repos(&self, org: &str) -> RunResult<Vec<String>>;

    async fn has_recent_commit(
        &self,
        org: &str,
        repo: &str,
        login: &str,
        field: &str,
        since: &str,
    ) -> RunResult<bool>;

    async fn make_repo_public(&self, org: &str, repo: &str) -> RunResult<()>;
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_repo_selection(raw: Option<String>, allow_star: bool) -> RepoSelection {
    let Some(raw) = raw else {
        return RepoSelection::Empty;
    };
    let trimmed = raw.trim();
    if allow_star && trimmed == "*" {
        return RepoSelection::AllPrivate;
    }
    let repos = split_csv(trimmed);
    if repos.is_empty() {
        RepoSelection::Empty
    } else {
        RepoSelection::Explicit(repos)
    }
}

fn parse_bool(raw: Option<String>) -> bool {
    matches!(
        raw.unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "armed" | "on"
    )
}

fn parse_u64(raw: Option<String>, fallback: u64) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn iso_from_millis(millis: i64) -> String {
    Utc.timestamp_millis_opt(millis)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn threshold_millis(now_millis: i64, inactive_days: u64) -> i64 {
    now_millis.saturating_sub((inactive_days as i64).saturating_mul(86_400_000))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let mut diff = left_bytes.len() ^ right_bytes.len();
    for index in 0..left_bytes.len().max(right_bytes.len()) {
        let a = left_bytes.get(index).copied().unwrap_or(0);
        let b = right_bytes.get(index).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }
    diff == 0
}

fn redacted_notes(config: &LastCommitConfig) -> Vec<String> {
    let mut notes = Vec::new();
    if config.trusted_logins.is_empty() {
        notes.push("TRUSTED_LOGINS is empty; LastCommit will not infer maintainers.".to_string());
    }
    if !config.release_repos.configured() {
        notes.push("RELEASE_REPOS is empty; no repositories can be made public.".to_string());
    }
    if matches!(config.watch_repos, RepoSelection::AllPrivate | RepoSelection::Empty) {
        notes.push(
            "WATCH_REPOS is not explicit; large orgs can exceed the Workers Free subrequest cap."
                .to_string(),
        );
    }
    if !config.armed {
        notes.push("LASTCOMMIT_ARMED is false; dead-man actions are dry-run only.".to_string());
    }
    notes
}

#[cfg(target_arch = "wasm32")]
use worker::{
    console_log, console_warn, event, Date, Env, Fetch, Headers, Method, Request, RequestInit,
    Response, ScheduleContext, ScheduledEvent,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn main(request: Request, env: Env, _ctx: worker::Context) -> worker::Result<Response> {
    console_error_panic_hook::set_once();

    let url = request.url()?;
    let path = url.path();

    match (request.method(), path) {
        (Method::Get, "/health") | (Method::Get, "/healthz") => health_response(&env),
        (Method::Get, "/dead") | (Method::Get, "/deadz") => match cached_deadman_response(&env).await
        {
            Ok(response) => Ok(response),
            Err(error) => json_error(&error, 500),
        },
        (Method::Post, "/run") => {
            if let Err(error) = authorize_admin(&request, &env) {
                return json_error(&error, 401);
            }
            match run_lastcommit(&env, ExecutionMode::Execute, "manual").await {
                Ok(mut report) => {
                    if let Err(error) = write_cached_report(&env, &report).await {
                        report
                            .notes
                            .push(format!("Failed to cache manual run status: {error}"));
                    }
                    json_response(&report, 200)
                }
                Err(error) => {
                    let failure = failure_status("manual", &error);
                    if let Err(cache_error) = write_cached_value(&env, &failure).await {
                        console_warn!(
                            "[lastcommit] failed to cache manual failure status: {}",
                            cache_error
                        );
                    }
                    json_error(&error, 500)
                }
            }
        }
        _ => json_error("Not found", 404),
    }
}

#[cfg(target_arch = "wasm32")]
#[event(scheduled)]
pub async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    console_error_panic_hook::set_once();

    let trigger = format!("cron:{}", event.cron());
    match run_lastcommit(&env, ExecutionMode::Execute, &trigger).await {
        Ok(report) => {
            if let Err(error) = write_cached_report(&env, &report).await {
                console_warn!("[lastcommit] failed to cache scheduled status: {}", error);
            }
            console_log!(
                "[lastcommit] {} checked {} repo(s), status {:?}, planned {}, executed {}",
                report.org,
                report.watched_repos.len(),
                report.status,
                report.planned_actions.len(),
                report.executed_actions.len()
            );
        }
        Err(error) => {
            let failure = failure_status(&trigger, &error);
            if let Err(cache_error) = write_cached_value(&env, &failure).await {
                console_warn!(
                    "[lastcommit] failed to cache scheduled failure status: {}",
                    cache_error
                );
            }
            console_warn!("[lastcommit] scheduled check failed: {}", error);
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn health_response(env: &Env) -> worker::Result<Response> {
    let config = load_config(env);
    let report = HealthReport {
        service: SERVICE_NAME,
        ok: !config.trusted_logins.is_empty() && config.release_repos.configured(),
        org: config.org.clone(),
        armed: config.armed,
        inactive_days: config.inactive_days,
        trusted_logins_configured: !config.trusted_logins.is_empty(),
        trusted_login_count: config.trusted_logins.len(),
        watch_repo_source: config.watch_repos.source(),
        release_repo_source: config.release_repos.source(),
        endpoints: vec!["GET /healthz", "GET /deadz", "GET /dead", "POST /run"],
        notes: redacted_notes(&config),
    };
    json_response(&report, 200)
}

#[cfg(target_arch = "wasm32")]
async fn cached_deadman_response(env: &Env) -> RunResult<Response> {
    let kv = env
        .kv(STATUS_KV_BINDING)
        .map_err(|error| format!("{STATUS_KV_BINDING} KV binding is not configured: {error}"))?;
    let Some(cached) = kv
        .get(STATUS_KV_KEY)
        .text()
        .await
        .map_err(|error| format!("Failed to read cached LastCommit status: {error}"))?
    else {
        return json_response(
            &serde_json::json!({
                "service": SERVICE_NAME,
                "ok": false,
                "status": "notCached",
                "message": "No cached LastCommit status exists yet. Wait for cron or POST /run.",
            }),
            503,
        )
        .map_err(|error| error.to_string());
    };

    let status = traffic_light_from_cached(&cached)?;
    json_response(&status, 200).map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn write_cached_report(env: &Env, report: &DeadmanReport) -> RunResult<()> {
    let value = serde_json::to_value(report)
        .map_err(|error| format!("Failed to serialize LastCommit report: {error}"))?;
    write_cached_value(env, &value).await
}

#[cfg(target_arch = "wasm32")]
async fn write_cached_value(env: &Env, value: &serde_json::Value) -> RunResult<()> {
    let kv = env
        .kv(STATUS_KV_BINDING)
        .map_err(|error| format!("{STATUS_KV_BINDING} KV binding is not configured: {error}"))?;
    kv.put(STATUS_KV_KEY, value.to_string())
        .map_err(|error| format!("Failed to prepare cached LastCommit status write: {error}"))?
        .execute()
        .await
        .map_err(|error| format!("Failed to cache LastCommit status: {error}"))
}

#[cfg(target_arch = "wasm32")]
fn failure_status(trigger: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "service": SERVICE_NAME,
        "ok": false,
        "trigger": trigger,
        "checkedAt": iso_from_millis(Date::now().as_millis() as i64),
        "status": "checkFailed",
        "error": error,
    })
}

fn traffic_light_from_cached(cached: &str) -> RunResult<TrafficLightStatus> {
    let value: serde_json::Value = serde_json::from_str(cached)
        .map_err(|error| format!("Cached LastCommit status is invalid JSON: {error}"))?;

    if value
        .get("status")
        .and_then(|status| status.as_str())
        .is_some_and(|status| status == "checkFailed")
    {
        return Ok(TrafficLightStatus {
            service: SERVICE_NAME,
            light: "yellow",
            status: "checkFailed",
            message: "LastCommit could not complete its last scheduled check.".to_string(),
            checked_at: value
                .get("checkedAt")
                .and_then(|checked_at| checked_at.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }

    let report: CachedDeadmanReport = serde_json::from_value(value)
        .map_err(|error| format!("Cached LastCommit status has an unexpected shape: {error}"))?;
    let checked_at = report.checked_at.clone();

    let status = report.status.unwrap_or(CachedDeadmanStatus::Blocked);
    let light = match status {
        CachedDeadmanStatus::Alive => "green",
        CachedDeadmanStatus::DeadDryRun | CachedDeadmanStatus::DeadExecuted => "red",
        CachedDeadmanStatus::Blocked => "yellow",
    };
    let status_text = match status {
        CachedDeadmanStatus::Alive => "alive",
        CachedDeadmanStatus::DeadDryRun => "deadDryRun",
        CachedDeadmanStatus::DeadExecuted => "deadExecuted",
        CachedDeadmanStatus::Blocked => "blocked",
    };
    let message = match status {
        CachedDeadmanStatus::Alive => "Trusted maintainer activity was found.".to_string(),
        CachedDeadmanStatus::DeadDryRun => {
            "No trusted maintainer activity was found; LastCommit is not armed.".to_string()
        }
        CachedDeadmanStatus::DeadExecuted => {
            "No trusted maintainer activity was found; continuity actions have run.".to_string()
        }
        CachedDeadmanStatus::Blocked => {
            "LastCommit is blocked by configuration or a failed setup check.".to_string()
        }
    };

    Ok(TrafficLightStatus {
        service: SERVICE_NAME,
        light,
        status: status_text,
        message,
        checked_at,
    })
}

#[cfg(target_arch = "wasm32")]
async fn run_lastcommit(
    env: &Env,
    mode: ExecutionMode,
    trigger: &str,
) -> RunResult<DeadmanReport> {
    let config = load_config(env);
    let token = read_secret(env, "GITHUB_TOKEN")?;
    let now_millis = Date::now().as_millis() as i64;
    let checked_at = iso_from_millis(now_millis);
    let threshold_at = iso_from_millis(threshold_millis(now_millis, config.inactive_days));
    let notes = redacted_notes(&config);

    if config.trusted_logins.is_empty() {
        return Ok(blocked_report(
            config,
            trigger,
            mode,
            checked_at,
            threshold_at,
            notes,
            "TRUSTED_LOGINS must name at least one GitHub login.",
        ));
    }

    let client = GithubClient::new(config.github_api_base.clone(), token);
    let watched_repos = resolve_repos(&client, &config, &config.watch_repos).await?;
    if watched_repos.is_empty() {
        return Ok(blocked_report(
            config,
            trigger,
            mode,
            checked_at,
            threshold_at,
            notes,
            "No watch repositories were resolved.",
        ));
    }

    let heartbeat = find_heartbeat(&client, &config, &watched_repos, &threshold_at).await?;
    if heartbeat.is_some() {
        return Ok(DeadmanReport {
            service: SERVICE_NAME,
            trigger: trigger.to_string(),
            mode: mode.as_str(),
            org: config.org.clone(),
            checked_at,
            inactive_days: config.inactive_days,
            threshold_at,
            armed: config.armed,
            status: DeadmanStatus::Alive,
            trusted_logins: config.trusted_logins.clone(),
            watch_repo_source: config.watch_repos.source(),
            watched_repos,
            release_repo_source: config.release_repos.source(),
            heartbeat,
            planned_actions: Vec::new(),
            executed_actions: Vec::new(),
            notes,
        });
    }

    if !config.release_repos.configured() {
        return Ok(blocked_report(
            config,
            trigger,
            mode,
            checked_at,
            threshold_at,
            notes,
            "RELEASE_REPOS must name repositories or '*' before LastCommit can act.",
        ));
    }

    let release_repos = resolve_repos(&client, &config, &config.release_repos).await?;
    let planned_actions: Vec<RepoAction> = release_repos
        .iter()
        .map(|repo| RepoAction {
            repo: repo.clone(),
            action: "makePublic",
            executed: false,
            ok: true,
            message: "Repository would be made public.".to_string(),
        })
        .collect();

    let should_execute = config.armed && matches!(mode, ExecutionMode::Execute);
    if !should_execute {
        return Ok(DeadmanReport {
            service: SERVICE_NAME,
            trigger: trigger.to_string(),
            mode: mode.as_str(),
            org: config.org.clone(),
            checked_at,
            inactive_days: config.inactive_days,
            threshold_at,
            armed: config.armed,
            status: DeadmanStatus::DeadDryRun,
            trusted_logins: config.trusted_logins.clone(),
            watch_repo_source: config.watch_repos.source(),
            watched_repos,
            release_repo_source: config.release_repos.source(),
            heartbeat: None,
            planned_actions,
            executed_actions: Vec::new(),
            notes,
        });
    }

    let executed_actions = execute_release_actions(&client, &config.org, &release_repos).await;

    Ok(DeadmanReport {
        service: SERVICE_NAME,
        trigger: trigger.to_string(),
        mode: mode.as_str(),
        org: config.org.clone(),
        checked_at,
        inactive_days: config.inactive_days,
        threshold_at,
        armed: config.armed,
        status: DeadmanStatus::DeadExecuted,
        trusted_logins: config.trusted_logins.clone(),
        watch_repo_source: config.watch_repos.source(),
        watched_repos,
        release_repo_source: config.release_repos.source(),
        heartbeat: None,
        planned_actions,
        executed_actions,
        notes,
    })
}

#[cfg(target_arch = "wasm32")]
fn blocked_report(
    config: LastCommitConfig,
    trigger: &str,
    mode: ExecutionMode,
    checked_at: String,
    threshold_at: String,
    mut notes: Vec<String>,
    message: &str,
) -> DeadmanReport {
    notes.push(message.to_string());
    DeadmanReport {
        service: SERVICE_NAME,
        trigger: trigger.to_string(),
        mode: mode.as_str(),
        org: config.org,
        checked_at,
        inactive_days: config.inactive_days,
        threshold_at,
        armed: config.armed,
        status: DeadmanStatus::Blocked,
        trusted_logins: config.trusted_logins,
        watch_repo_source: config.watch_repos.source(),
        watched_repos: Vec::new(),
        release_repo_source: config.release_repos.source(),
        heartbeat: None,
        planned_actions: Vec::new(),
        executed_actions: Vec::new(),
        notes,
    }
}

async fn resolve_repos(
    client: &impl GithubApi,
    config: &LastCommitConfig,
    selection: &RepoSelection,
) -> RunResult<Vec<String>> {
    match selection {
        RepoSelection::Explicit(repos) => Ok(repos.clone()),
        RepoSelection::AllPrivate | RepoSelection::Empty => client.list_private_repos(&config.org).await,
    }
}

async fn find_heartbeat(
    client: &impl GithubApi,
    config: &LastCommitConfig,
    repos: &[String],
    since: &str,
) -> RunResult<Option<Heartbeat>> {
    for repo in repos {
        for login in &config.trusted_logins {
            if client.has_recent_commit(&config.org, repo, login, "author", since).await? {
                return Ok(Some(Heartbeat {
                    login: login.clone(),
                    repo: repo.clone(),
                    match_kind: "author".to_string(),
                    since: since.to_string(),
                }));
            }
            if client.has_recent_commit(&config.org, repo, login, "committer", since).await? {
                return Ok(Some(Heartbeat {
                    login: login.clone(),
                    repo: repo.clone(),
                    match_kind: "committer".to_string(),
                    since: since.to_string(),
                }));
            }
        }
    }
    Ok(None)
}

async fn execute_release_actions(
    client: &impl GithubApi,
    org: &str,
    repos: &[String],
) -> Vec<RepoAction> {
    let mut executed_actions = Vec::new();
    for repo in repos {
        match client.make_repo_public(org, repo).await {
            Ok(()) => executed_actions.push(RepoAction {
                repo: repo.clone(),
                action: "makePublic",
                executed: true,
                ok: true,
                message: "Repository was made public.".to_string(),
            }),
            Err(error) => executed_actions.push(RepoAction {
                repo: repo.clone(),
                action: "makePublic",
                executed: true,
                ok: false,
                message: error,
            }),
        }
    }
    executed_actions
}

#[cfg(target_arch = "wasm32")]
fn load_config(env: &Env) -> LastCommitConfig {
    LastCommitConfig {
        org: read_var(env, "GITHUB_ORG").unwrap_or_else(|| DEFAULT_GITHUB_ORG.to_string()),
        inactive_days: parse_u64(read_var(env, "INACTIVE_DAYS"), DEFAULT_INACTIVE_DAYS),
        trusted_logins: split_csv(&read_var(env, "TRUSTED_LOGINS").unwrap_or_default()),
        watch_repos: parse_repo_selection(read_var(env, "WATCH_REPOS"), false),
        release_repos: parse_repo_selection(read_var(env, "RELEASE_REPOS"), true),
        armed: parse_bool(read_var(env, "LASTCOMMIT_ARMED")),
        github_api_base: read_var(env, "GITHUB_API_BASE")
            .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE.to_string()),
    }
}

#[cfg(target_arch = "wasm32")]
fn read_var(env: &Env, name: &str) -> Option<String> {
    env.var(name)
        .ok()
        .map(|value| value.to_string())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_arch = "wasm32")]
fn read_secret(env: &Env, name: &str) -> RunResult<String> {
    env.secret(name)
        .map(|value| value.to_string())
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("{name} secret is not configured"))
        .and_then(|value| {
            if value.is_empty() {
                Err(format!("{name} secret is empty"))
            } else {
                Ok(value)
            }
        })
}

#[cfg(target_arch = "wasm32")]
fn authorize_admin(request: &Request, env: &Env) -> RunResult<()> {
    let expected = read_secret(env, "LASTCOMMIT_ADMIN_TOKEN")
        .map_err(|_| "LASTCOMMIT_ADMIN_TOKEN secret is not configured".to_string())?;
    let bearer = format!("Bearer {expected}");
    let auth_header = request
        .headers()
        .get("Authorization")
        .map_err(|_| "Invalid Authorization header".to_string())?
        .unwrap_or_default();
    let token_header = request
        .headers()
        .get("X-LastCommit-Admin")
        .map_err(|_| "Invalid X-LastCommit-Admin header".to_string())?
        .unwrap_or_default();

    if constant_time_eq(&auth_header, &bearer) || constant_time_eq(&token_header, &expected) {
        Ok(())
    } else {
        Err("Unauthorized".to_string())
    }
}

#[cfg(target_arch = "wasm32")]
struct GithubClient {
    base_url: String,
    token: String,
}

#[cfg(target_arch = "wasm32")]
impl GithubClient {
    fn new(base_url: String, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    async fn list_private_repos(&self, org: &str) -> RunResult<Vec<String>> {
        let mut repos = Vec::new();
        for page in 1..=MAX_REPO_PAGES {
            let path = format!(
                "/orgs/{}/repos?type=private&per_page=100&page={}",
                encode_path(org),
                page
            );
            let page_repos: Vec<GithubRepo> = self.fetch_json(Method::Get, &path, None).await?;
            let count = page_repos.len();
            repos.extend(
                page_repos
                    .into_iter()
                    .filter(|repo| repo.private)
                    .map(|repo| repo.name),
            );
            if count < 100 {
                break;
            }
        }
        repos.sort();
        repos.dedup();
        Ok(repos)
    }

    async fn has_recent_commit(
        &self,
        org: &str,
        repo: &str,
        login: &str,
        field: &str,
        since: &str,
    ) -> RunResult<bool> {
        let path = format!(
            "/repos/{}/{}/commits?since={}&{}={}&per_page=1",
            encode_path(org),
            encode_path(repo),
            urlencoding::encode(since),
            field,
            urlencoding::encode(login)
        );
        let commits: Vec<GithubCommit> = self.fetch_json(Method::Get, &path, None).await?;
        Ok(commits.into_iter().any(|commit| match field {
            "author" => commit
                .author
                .map(|author| author.login.eq_ignore_ascii_case(login))
                .unwrap_or(false),
            "committer" => commit
                .committer
                .map(|committer| committer.login.eq_ignore_ascii_case(login))
                .unwrap_or(false),
            _ => false,
        }))
    }

    async fn make_repo_public(&self, org: &str, repo: &str) -> RunResult<()> {
        let path = format!("/repos/{}/{}", encode_path(org), encode_path(repo));
        let _: serde_json::Value = self
            .fetch_json(
                Method::Patch,
                &path,
                Some(serde_json::json!({
                    "private": false
                })),
            )
            .await?;
        Ok(())
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> RunResult<T> {
        let url = format!("{}{}", self.base_url, path);
        let headers = Headers::new();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .map_err(|error| error.to_string())?;
        headers
            .set("Accept", "application/vnd.github+json")
            .map_err(|error| error.to_string())?;
        headers
            .set("X-GitHub-Api-Version", "2022-11-28")
            .map_err(|error| error.to_string())?;
        headers
            .set("User-Agent", "LastCommit")
            .map_err(|error| error.to_string())?;

        let mut init = RequestInit::new();
        init.with_method(method.clone()).with_headers(headers);
        if let Some(body) = body {
            init.headers
                .set("Content-Type", "application/json")
                .map_err(|error| error.to_string())?;
            init.with_body(Some(JsValue::from_str(&body.to_string())));
        }

        let request = Request::new_with_init(&url, &init).map_err(|error| error.to_string())?;
        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status_code();
        let text = response.text().await.map_err(|error| error.to_string())?;

        if !(200..=299).contains(&status) {
            return Err(format!(
                "GitHub {} {} returned {}: {}",
                method.as_ref(),
                path,
                status,
                truncate(&text, 320)
            ));
        }

        serde_json::from_str(&text).map_err(|error| {
            format!(
                "GitHub {} {} returned invalid JSON: {}",
                method.as_ref(),
                path,
                error
            )
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl GithubApi for GithubClient {
    async fn list_private_repos(&self, org: &str) -> RunResult<Vec<String>> {
        GithubClient::list_private_repos(self, org).await
    }

    async fn has_recent_commit(
        &self,
        org: &str,
        repo: &str,
        login: &str,
        field: &str,
        since: &str,
    ) -> RunResult<bool> {
        GithubClient::has_recent_commit(self, org, repo, login, field, since).await
    }

    async fn make_repo_public(&self, org: &str, repo: &str) -> RunResult<()> {
        GithubClient::make_repo_public(self, org, repo).await
    }
}

fn encode_path(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for character in value.chars().take(max_chars) {
        output.push(character);
    }
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

#[cfg(target_arch = "wasm32")]
fn json_response<T: Serialize>(value: &T, status: u16) -> worker::Result<Response> {
    let headers = Headers::new();
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::from_json(value)?
        .with_status(status)
        .with_headers(headers))
}

#[cfg(target_arch = "wasm32")]
fn json_error(message: &str, status: u16) -> worker::Result<Response> {
    json_response(
        &serde_json::json!({
            "service": SERVICE_NAME,
            "ok": false,
            "error": message,
        }),
        status,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Default)]
    struct MockGithub {
        private_repos: Vec<String>,
        recent_commits: Vec<RecentCommit>,
        failed_public_repos: Vec<String>,
        listed_private_repos: Cell<usize>,
        made_public_repos: RefCell<Vec<String>>,
    }

    #[derive(Debug, Clone)]
    struct RecentCommit {
        repo: String,
        login: String,
        field: String,
    }

    impl MockGithub {
        fn with_private_repos(mut self, repos: &[&str]) -> Self {
            self.private_repos = repos.iter().map(|repo| repo.to_string()).collect();
            self
        }

        fn with_recent_commit(mut self, repo: &str, login: &str, field: &str) -> Self {
            self.recent_commits.push(RecentCommit {
                repo: repo.to_string(),
                login: login.to_string(),
                field: field.to_string(),
            });
            self
        }

        fn with_public_failure(mut self, repo: &str) -> Self {
            self.failed_public_repos.push(repo.to_string());
            self
        }
    }

    impl GithubApi for MockGithub {
        async fn list_private_repos(&self, _org: &str) -> RunResult<Vec<String>> {
            self.listed_private_repos
                .set(self.listed_private_repos.get() + 1);
            Ok(self.private_repos.clone())
        }

        async fn has_recent_commit(
            &self,
            _org: &str,
            repo: &str,
            login: &str,
            field: &str,
            _since: &str,
        ) -> RunResult<bool> {
            Ok(self.recent_commits.iter().any(|commit| {
                commit.repo == repo
                    && commit.login == login
                    && commit.field == field
            }))
        }

        async fn make_repo_public(&self, _org: &str, repo: &str) -> RunResult<()> {
            self.made_public_repos.borrow_mut().push(repo.to_string());
            if self.failed_public_repos.iter().any(|failed| failed == repo) {
                Err(format!("{repo} failed"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn split_csv_trims_empty_values() {
        assert_eq!(
            split_csv(" jamie, , wavey-ai-bot ,,"),
            vec!["jamie".to_string(), "wavey-ai-bot".to_string()]
        );
    }

    #[test]
    fn repo_selection_all_private_requires_star() {
        assert!(matches!(
            parse_repo_selection(Some("*".to_string()), true),
            RepoSelection::AllPrivate
        ));
        assert!(matches!(
            parse_repo_selection(Some("*".to_string()), false),
            RepoSelection::Explicit(_)
        ));
    }

    #[test]
    fn bool_parser_accepts_armed_terms() {
        assert!(parse_bool(Some("armed".to_string())));
        assert!(parse_bool(Some("TRUE".to_string())));
        assert!(!parse_bool(Some("false".to_string())));
        assert!(!parse_bool(None));
    }

    #[test]
    fn threshold_uses_days_in_milliseconds() {
        assert_eq!(threshold_millis(172_800_000, 1), 86_400_000);
    }

    #[test]
    fn constant_time_eq_checks_content_and_length() {
        assert!(constant_time_eq("Bearer abc", "Bearer abc"));
        assert!(!constant_time_eq("Bearer abc", "Bearer abd"));
        assert!(!constant_time_eq("Bearer abc", "Bearer abc "));
    }

    #[test]
    fn cached_alive_report_becomes_public_green_light() {
        let cached = serde_json::json!({
            "service": SERVICE_NAME,
            "trigger": "cron:17 9 * * *",
            "mode": "execute",
            "org": "wavey-ai",
            "checkedAt": "2026-06-04T09:17:00Z",
            "inactiveDays": 180,
            "thresholdAt": "2025-12-06T09:17:00Z",
            "armed": false,
            "status": "alive",
            "trustedLogins": ["jamie"],
            "watchRepoSource": "explicit",
            "watchedRepos": ["secret-repo"],
            "releaseRepoSource": "explicit",
            "heartbeat": {
                "login": "jamie",
                "repo": "secret-repo",
                "matchKind": "author",
                "since": "2025-12-06T09:17:00Z"
            },
            "plannedActions": [],
            "executedActions": [],
            "notes": []
        });

        let status = traffic_light_from_cached(&cached.to_string()).unwrap();

        assert_eq!(status.light, "green");
        assert_eq!(status.status, "alive");
        assert_eq!(status.checked_at, "2026-06-04T09:17:00Z");
        assert!(!status.message.contains("wavey-ai"));
        assert!(!status.message.contains("secret-repo"));
        assert!(!status.message.contains("jamie"));
    }

    #[test]
    fn cached_dead_dry_run_report_becomes_public_red_light() {
        let cached = serde_json::json!({
            "service": SERVICE_NAME,
            "checkedAt": "2026-06-04T09:17:00Z",
            "status": "deadDryRun",
            "plannedActions": [{ "repo": "private-one" }]
        });

        let status = traffic_light_from_cached(&cached.to_string()).unwrap();

        assert_eq!(status.light, "red");
        assert_eq!(status.status, "deadDryRun");
        assert_eq!(
            status.message,
            "No trusted maintainer activity was found; LastCommit is not armed."
        );
        assert!(!status.message.contains("private-one"));
    }

    #[test]
    fn cached_failure_report_becomes_public_yellow_light() {
        let cached = serde_json::json!({
            "service": SERVICE_NAME,
            "ok": false,
            "trigger": "cron:17 9 * * *",
            "checkedAt": "2026-06-04T09:17:00Z",
            "status": "checkFailed",
            "error": "GitHub returned 500"
        });

        let status = traffic_light_from_cached(&cached.to_string()).unwrap();

        assert_eq!(status.light, "yellow");
        assert_eq!(status.status, "checkFailed");
        assert!(!status.message.contains("GitHub returned 500"));
    }

    #[test]
    fn explicit_repo_selection_does_not_scan_all_private_repos() {
        let client = MockGithub::default().with_private_repos(&["private-one"]);
        let config = LastCommitConfig {
            org: "wavey-ai".to_string(),
            inactive_days: 180,
            trusted_logins: vec!["jamie".to_string()],
            watch_repos: RepoSelection::Explicit(vec!["watch-one".to_string()]),
            release_repos: RepoSelection::Empty,
            armed: false,
            github_api_base: DEFAULT_GITHUB_API_BASE.to_string(),
        };

        let repos = futures::executor::block_on(resolve_repos(
            &client,
            &config,
            &config.watch_repos,
        ))
        .unwrap();

        assert_eq!(repos, vec!["watch-one".to_string()]);
        assert_eq!(client.listed_private_repos.get(), 0);
    }

    #[test]
    fn all_private_repo_selection_uses_github_repo_listing() {
        let client = MockGithub::default().with_private_repos(&["private-one", "private-two"]);
        let config = LastCommitConfig {
            org: "wavey-ai".to_string(),
            inactive_days: 180,
            trusted_logins: vec!["jamie".to_string()],
            watch_repos: RepoSelection::AllPrivate,
            release_repos: RepoSelection::Empty,
            armed: false,
            github_api_base: DEFAULT_GITHUB_API_BASE.to_string(),
        };

        let repos = futures::executor::block_on(resolve_repos(
            &client,
            &config,
            &config.watch_repos,
        ))
        .unwrap();

        assert_eq!(repos, vec!["private-one".to_string(), "private-two".to_string()]);
        assert_eq!(client.listed_private_repos.get(), 1);
    }

    #[test]
    fn heartbeat_detection_checks_author_and_committer() {
        let client =
            MockGithub::default().with_recent_commit("watch-one", "jamie", "committer");
        let config = LastCommitConfig {
            org: "wavey-ai".to_string(),
            inactive_days: 180,
            trusted_logins: vec!["jamie".to_string()],
            watch_repos: RepoSelection::Explicit(vec!["watch-one".to_string()]),
            release_repos: RepoSelection::Empty,
            armed: false,
            github_api_base: DEFAULT_GITHUB_API_BASE.to_string(),
        };

        let heartbeat = futures::executor::block_on(find_heartbeat(
            &client,
            &config,
            &["watch-one".to_string()],
            "2025-12-06T09:17:00Z",
        ))
        .unwrap()
        .unwrap();

        assert_eq!(heartbeat.login, "jamie");
        assert_eq!(heartbeat.repo, "watch-one");
        assert_eq!(heartbeat.match_kind, "committer");
    }

    #[test]
    fn heartbeat_detection_returns_none_when_no_trusted_commit_exists() {
        let client =
            MockGithub::default().with_recent_commit("watch-one", "someone-else", "author");
        let config = LastCommitConfig {
            org: "wavey-ai".to_string(),
            inactive_days: 180,
            trusted_logins: vec!["jamie".to_string()],
            watch_repos: RepoSelection::Explicit(vec!["watch-one".to_string()]),
            release_repos: RepoSelection::Empty,
            armed: false,
            github_api_base: DEFAULT_GITHUB_API_BASE.to_string(),
        };

        let heartbeat = futures::executor::block_on(find_heartbeat(
            &client,
            &config,
            &["watch-one".to_string()],
            "2025-12-06T09:17:00Z",
        ))
        .unwrap();

        assert!(heartbeat.is_none());
    }

    #[test]
    fn release_actions_collect_successes_and_failures() {
        let client = MockGithub::default().with_public_failure("private-two");

        let actions = futures::executor::block_on(execute_release_actions(
            &client,
            "wavey-ai",
            &["private-one".to_string(), "private-two".to_string()],
        ));

        assert_eq!(
            client.made_public_repos.borrow().clone(),
            vec!["private-one".to_string(), "private-two".to_string()]
        );
        assert_eq!(actions.len(), 2);
        assert!(actions[0].ok);
        assert_eq!(actions[0].message, "Repository was made public.");
        assert!(!actions[1].ok);
        assert_eq!(actions[1].message, "private-two failed");
    }
}
