use std::env;
use std::io::Read;

const DEFAULT_WORKER_BASE: &str = "http://localhost:8787";

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    content_type: String,
    body: String,
}

#[test]
#[ignore = "requires a running LastCommit Worker, for example `npm run dev`"]
fn root_returns_switch_splash_html() {
    let response = http_call(ureq::get(&worker_base()));

    assert_eq!(response.status, 200, "root status");
    assert!(
        response.content_type.starts_with("text/html"),
        "root content type should be HTML, got {}",
        response.content_type
    );
    assert!(
        response.body.contains("/lastcommit-switch.png"),
        "splash HTML should reference the switch image"
    );
    assert!(
        response
            .body
            .contains("https://github.com/wavey-ai/lastcommit"),
        "splash switch should link to the GitHub repo"
    );
    assert!(
        response.body.contains("heartbeat-glow"),
        "splash HTML should include the heartbeat-like pulse animation"
    );
}

#[test]
#[ignore = "requires a running LastCommit Worker, for example `npm run dev`"]
fn healthz_returns_liveness_json() {
    let response = http_call(ureq::get(&format!("{}/healthz", worker_base())));

    assert_eq!(response.status, 200, "healthz status");
    assert!(
        response.content_type.starts_with("application/json"),
        "healthz content type should be JSON, got {}",
        response.content_type
    );
    let json: serde_json::Value = serde_json::from_str(&response.body).expect("healthz JSON");
    assert_eq!(
        json.get("service").and_then(|value| value.as_str()),
        Some("LastCommit")
    );
    assert_eq!(
        json.get("endpoints")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(5)
    );
}

#[test]
#[ignore = "requires a running LastCommit Worker with LASTCOMMIT_STATUS KV bound"]
fn deadz_returns_public_traffic_light_or_uncached_status() {
    let response = http_call(ureq::get(&format!("{}/deadz", worker_base())));

    assert!(
        matches!(response.status, 200 | 503),
        "deadz should return cached traffic light or uncached 503, got {}",
        response.status
    );
    let json: serde_json::Value = serde_json::from_str(&response.body).expect("deadz JSON");
    assert_eq!(
        json.get("service").and_then(|value| value.as_str()),
        Some("LastCommit")
    );

    if response.status == 200 {
        let light = json
            .get("light")
            .and_then(|value| value.as_str())
            .expect("traffic-light response should include light");
        assert!(
            matches!(light, "green" | "yellow" | "red"),
            "unexpected light {light}"
        );
        assert!(
            json.get("message")
                .and_then(|value| value.as_str())
                .is_some(),
            "traffic-light response should include a public message"
        );
    } else {
        assert_eq!(
            json.get("status").and_then(|value| value.as_str()),
            Some("notCached")
        );
    }
}

#[test]
#[ignore = "requires a running LastCommit Worker"]
fn run_rejects_missing_admin_token() {
    let response = http_call(ureq::post(&format!("{}/run", worker_base())));

    assert_eq!(response.status, 401, "run without auth should be rejected");
    let json: serde_json::Value = serde_json::from_str(&response.body).expect("run JSON");
    assert_eq!(
        json.get("service").and_then(|value| value.as_str()),
        Some("LastCommit")
    );
    assert_eq!(
        json.get("ok").and_then(|value| value.as_bool()),
        Some(false)
    );
}

fn worker_base() -> String {
    env::var("LASTCOMMIT_WORKER_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_WORKER_BASE.to_string())
}

fn http_call(request: ureq::Request) -> HttpResponse {
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("HTTP request failed: {error}"),
    };
    let status = response.status();
    let content_type = response.header("Content-Type").unwrap_or("").to_string();
    let mut body = String::new();
    response
        .into_reader()
        .read_to_string(&mut body)
        .expect("read response body");
    HttpResponse {
        status,
        content_type,
        body,
    }
}
