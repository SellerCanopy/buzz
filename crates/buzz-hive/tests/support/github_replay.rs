//! A local stand-in for the GitHub REST API, in the manner of kiln's `fake-github.mjs`.
//!
//! A test lists the exchanges it expects. A request is answered from the first exchange that
//! matches its method, path and the query pairs the exchange names (other pairs are ignored).
//! Ordered exchanges are answered in the order listed; an `any_order` exchange is answered
//! whenever it matches; a request that repeats the one before is answered with the same
//! reply again. A request nothing matches is answered `418`, and [`Replay::finish`] fails the
//! test naming every such request, and every exchange that was never asked for. Nothing here
//! reaches the network: it listens on a port the system picks on the loopback address.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;

#[derive(Clone)]
pub struct Exchange {
    method: Method,
    path: String,
    query: Vec<(String, String)>,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    any_order: bool,
    used: bool,
}

impl Exchange {
    pub fn get(path: &str) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn post(path: &str) -> Self {
        Self::new(Method::POST, path)
    }

    fn new(method: Method, path: &str) -> Self {
        Self {
            method,
            path: path.to_owned(),
            query: Vec::new(),
            status: 200,
            headers: Vec::new(),
            body: "{}".to_owned(),
            any_order: false,
            used: false,
        }
    }

    /// The request must carry this query pair.
    pub fn query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Answers with the recorded reply in `tests/fixtures/github/<name>`.
    pub fn fixture(mut self, name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/github")
            .join(name);
        self.body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read the fixture {}: {e}", path.display()));
        self
    }

    pub fn body(mut self, body: &str) -> Self {
        self.body = body.to_owned();
        self
    }

    /// Answered whenever it matches, in any position.
    pub fn any_order(mut self) -> Self {
        self.any_order = true;
        self
    }

    fn matches(&self, method: &Method, path: &str, query: &[(String, String)]) -> bool {
        self.method == *method
            && self.path == path
            && self.query.iter().all(|pair| query.contains(pair))
    }

    fn describe(&self) -> String {
        let query: Vec<String> = self.query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if query.is_empty() {
            format!("{} {}", self.method, self.path)
        } else {
            format!("{} {}?{}", self.method, self.path, query.join("&"))
        }
    }
}

/// One request the server got.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct Inner {
    ordered: VecDeque<Exchange>,
    any: Vec<Exchange>,
    last: Option<Exchange>,
    seen: Vec<Seen>,
    unknown: Vec<String>,
}

pub struct Replay {
    url: String,
    inner: Arc<Mutex<Inner>>,
    task: tokio::task::JoinHandle<()>,
}

impl Replay {
    pub async fn start(exchanges: Vec<Exchange>) -> Self {
        let mut inner = Inner::default();
        for exchange in exchanges {
            if exchange.any_order {
                inner.any.push(exchange);
            } else {
                inner.ordered.push_back(exchange);
            }
        }
        let inner = Arc::new(Mutex::new(inner));
        let router = Router::new().fallback(answer).with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self { url, inner, task }
    }

    /// The address to give the adapter as the API address.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().seen.clone()
    }

    /// Stops the server and fails the test for every request it had no answer for and every
    /// exchange nobody asked for.
    pub fn finish(self) {
        self.task.abort();
        let inner = self.inner.lock().unwrap();
        let mut problems = Vec::new();
        for request in &inner.unknown {
            problems.push(format!("unknown request, no exchange answers {request}"));
        }
        for exchange in &inner.ordered {
            problems.push(format!("never asked for: {}", exchange.describe()));
        }
        for exchange in inner.any.iter().filter(|e| !e.used) {
            problems.push(format!("never asked for: {}", exchange.describe()));
        }
        if !problems.is_empty() {
            panic!("the GitHub replay server: {}", problems.join("; "));
        }
    }
}

async fn answer(
    State(inner): State<Arc<Mutex<Inner>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let query: Vec<(String, String)> = reqwest::Url::parse(&format!("http://replay{uri}"))
        .map(|url| {
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let path = uri.path().to_owned();
    let mut inner = inner.lock().unwrap();
    inner.seen.push(Seen {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned()))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });

    let found = if inner
        .ordered
        .front()
        .is_some_and(|e| e.matches(&method, &path, &query))
    {
        let exchange = inner.ordered.pop_front().unwrap();
        inner.last = Some(exchange.clone());
        Some(exchange)
    } else if let Some(at) = inner
        .any
        .iter()
        .position(|e| e.matches(&method, &path, &query))
    {
        inner.any[at].used = true;
        Some(inner.any[at].clone())
    } else if inner
        .last
        .as_ref()
        .is_some_and(|e| e.matches(&method, &path, &query))
    {
        inner.last.clone()
    } else {
        None
    };
    match found {
        Some(exchange) => {
            let mut response = (
                StatusCode::from_u16(exchange.status).unwrap(),
                exchange.body.clone(),
            )
                .into_response();
            for (name, value) in &exchange.headers {
                response.headers_mut().insert(
                    axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    axum::http::HeaderValue::from_str(value).unwrap(),
                );
            }
            response
        }
        None => {
            let line = format!("{method} {}", uri);
            inner.unknown.push(line.clone());
            (StatusCode::IM_A_TEAPOT, format!("no exchange for {line}")).into_response()
        }
    }
}
