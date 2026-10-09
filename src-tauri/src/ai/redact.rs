//! Request URLs never reach the user's screen. reqwest prints the URL in the text of every
//! transport error -- "error sending request for url (...)" -- and the panel shows an error it
//! has no code for raw. Our own clients keep keys in headers (v0.7.36), but a custom base URL is
//! the user's to write, and some gateways take the key in the query or as `user:pass@`. So an
//! error that mentions a URL keeps only its scheme and host, which is what diagnosing a failure
//! needs; the path, query and userinfo are dropped.

/// Applied to every provider call in `AiRouter::send_guidance_request`. An error with no URL in
/// it is returned untouched, so the bare codes the panel maps (`free_trial_exhausted`, ...) and
/// the error chain both survive. One with a URL becomes its full chain (`{:#}`), redacted --
/// which also shows the cause ("operation timed out") that reqwest's top line leaves out.
pub fn request_urls(e: anyhow::Error) -> anyhow::Error {
    let full = format!("{e:#}");
    if !full.contains("://") {
        return e;
    }
    anyhow::Error::msg(redact_urls(&full))
}

/// `https://user:pw@host:8080/v1/x?key=K` -> `https://host:8080/...`. A URL ends at whitespace,
/// a closing bracket or a quote -- none of which an API key contains.
pub fn redact_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = next_url(rest) {
        out.push_str(&rest[..start]);
        let at = &rest[start..];
        let scheme_end = at.find("://").map_or(0, |i| i + 3);
        let tail = &at[scheme_end..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '<' | '>'))
            .unwrap_or(tail.len());
        let url = &tail[..end];
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let authority = &url[..authority_end];
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        out.push_str(&at[..scheme_end]);
        out.push_str(host);
        if authority_end < url.len() {
            out.push_str("/...");
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn next_url(s: &str) -> Option<usize> {
    match (s.find("https://"), s.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_in_the_query_or_userinfo_never_survives() {
        let e = "error sending request for url (https://gw.example.com/v1/chat?alt=sse&key=SECRET1)";
        assert_eq!(redact_urls(e), "error sending request for url (https://gw.example.com/...)");
        let e = "dns error for https://me:SECRET2@proxy.example.com:8443/openai/v1 and http://localhost:11434";
        assert_eq!(
            redact_urls(e),
            "dns error for https://proxy.example.com:8443/... and http://localhost:11434"
        );
        let e = r#"{"error":"see https://x.example.com/p?token=SECRET3"}"#;
        assert_eq!(redact_urls(e), r#"{"error":"see https://x.example.com/..."}"#);
    }

    #[test]
    fn an_error_without_a_url_is_left_exactly_as_it_was() {
        let e = request_urls(anyhow::anyhow!("free_trial_exhausted"));
        assert_eq!(e.to_string(), "free_trial_exhausted");
        let e = request_urls(anyhow::anyhow!("inner").context("outer"));
        assert_eq!(e.to_string(), "outer");
        assert_eq!(format!("{e:#}"), "outer: inner");
    }

    // The real thing: a reqwest timeout against a listener that never answers, with a key in
    // the query the way the Gemini client used to send it.
    #[test]
    fn a_real_reqwest_timeout_loses_the_key_and_keeps_the_host_and_cause() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt.block_on(async {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(200))
                .build()
                .unwrap()
                .get(format!("http://127.0.0.1:{port}/v1/models?key=SECRET4"))
                .send()
                .await
                .unwrap_err()
        });
        assert!(err.to_string().contains("SECRET4"), "precondition: reqwest prints the URL");
        let shown = request_urls(anyhow::Error::new(err)).to_string();
        assert!(!shown.contains("SECRET4"), "{shown}");
        assert!(shown.contains(&format!("http://127.0.0.1:{port}/...")), "{shown}");
        assert!(shown.to_lowercase().contains("timed out"), "{shown}");
        drop(listener);
    }

    // End to end through the router, as `guide` calls it: a custom base URL carrying a key,
    // pointed at a listener that never answers. `to_string()` is what reaches the panel.
    #[test]
    fn the_router_puts_no_custom_base_url_credential_on_screen() {
        use crate::ai::{config::Config, cost_tracker::CostTracker, router::AiRouter, session::SessionManager};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = Config::default();
        config.api_provider = "custom".into();
        config.custom_base_url = format!("http://127.0.0.1:{port}/v1?key=SECRET5");
        config.custom_model = "m".into();
        config.api_timeout_sec = 1;
        let dir = std::env::temp_dir().join(format!("navisual-redact-test-{}", std::process::id()));
        let mut router = AiRouter::new(config, CostTracker::new(None), SessionManager::new(dir), None);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt
            .block_on(router.send_guidance_request("task", None, None, |_, _| {}))
            .unwrap_err();
        let shown = err.to_string();
        assert!(!shown.contains("SECRET5"), "{shown}");
        // The host is only in the text if the URL was -- so this is not passing vacuously.
        assert!(shown.contains(&format!("127.0.0.1:{port}")), "{shown}");
        drop(listener);
    }
}
