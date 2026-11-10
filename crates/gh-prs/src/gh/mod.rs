//! Everything that talks to GitHub, always through the `gh` CLI and the
//! user's existing `gh auth` session.
//!
//! "Error" must never look like "nothing to do": every failure, including a
//! response that deviates from the expected envelope, is a [`GhError`].

mod fetch;
mod parse;
mod queries;

use std::io::{ErrorKind, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value};
use wait_timeout::ChildExt;

pub use fetch::{
    REVIEW_PAGE_LIMIT, SEARCH_LIMIT, approve_pr, count_prs, fetch_pr, fetch_pr_head, fetch_prs,
    merge_pr, resolve_pr,
};

/// A `gh` invocation failed: missing binary, auth, network, or bad output.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GhError(pub String);

/// What a `gh` run produced.
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// gh's own explanation of a failure: stderr, else stdout.
    fn detail(&self) -> &str {
        let stderr = self.stderr.trim();
        if stderr.is_empty() {
            self.stdout.trim()
        } else {
            stderr
        }
    }
}

/// Runs `gh` with the given arguments. The seam tests replace: everything
/// above it is plain code over canned output.
pub trait Runner: Sync {
    fn run(&self, args: &[String]) -> Result<Output, GhError>;
}

impl<F: Fn(&[String]) -> Result<Output, GhError> + Sync> Runner for F {
    fn run(&self, args: &[String]) -> Result<Output, GhError> {
        self(args)
    }
}

/// The real `gh`, killed after [`GH_TIMEOUT`] so a stalled network can't
/// hang a status bar forever.
pub struct GhCli;

pub const GH_TIMEOUT: Duration = Duration::from_secs(60);

impl Runner for GhCli {
    fn run(&self, args: &[String]) -> Result<Output, GhError> {
        let mut child = Command::new("gh")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == ErrorKind::NotFound {
                    GhError("gh CLI not found. Install it from https://cli.github.com/".to_owned())
                } else {
                    GhError(format!("Failed to run gh: {e}"))
                }
            })?;
        // Both pipes are drained on their own threads, or a chatty gh could
        // fill one and block before exiting.
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                String::from_utf8_lossy(&bytes).into_owned()
            })
        };
        let stdout = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let stderr = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let status = match child.wait_timeout(GH_TIMEOUT) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GhError(format!(
                    "gh timed out after {}s (network stalled?)",
                    GH_TIMEOUT.as_secs()
                )));
            }
            Err(e) => return Err(GhError(format!("Failed to run gh: {e}"))),
        };
        let joined = |reader: thread::JoinHandle<String>| {
            reader
                .join()
                .map_err(|_| GhError("Failed to run gh: output reader crashed".to_owned()))
        };
        Ok(Output {
            success: status.success(),
            stdout: joined(stdout)?,
            stderr: joined(stderr)?,
        })
    }
}

/// Run `gh api graphql` and return its validated `data` block.
///
/// Every deviation from the expected envelope is an error: a mangled or
/// drifted response must never read as an empty result.
fn graphql(
    runner: &dyn Runner,
    context: &str,
    args: &[String],
) -> Result<Map<String, Value>, GhError> {
    let mut full = vec!["api".to_owned(), "graphql".to_owned()];
    full.extend_from_slice(args);
    let output = runner.run(&full)?;
    if !output.success {
        return Err(GhError(format!("{context} failed: {}", output.detail())));
    }
    let payload: Value = serde_json::from_str(&output.stdout)
        .map_err(|e| GhError(format!("{context}: invalid JSON from gh: {e}")))?;
    let Value::Object(mut payload) = payload else {
        return Err(GhError(format!(
            "{context}: unexpected response from gh (not a JSON object)"
        )));
    };
    if let Some(errors) = payload.get("errors").filter(|errors| is_truthy(errors)) {
        let messages: Vec<String> = errors
            .as_array()
            .map(|errors| errors.iter().map(error_message).collect())
            .unwrap_or_else(|| vec![error_message(errors)]);
        return Err(GhError(format!(
            "{context} returned errors: {}",
            messages.join("; ")
        )));
    }
    match payload.remove("data") {
        Some(Value::Object(data)) => Ok(data),
        Some(_) | None => Err(GhError(format!(
            "{context}: response has no data block (unexpected shape)"
        ))),
    }
}

/// JSON truthiness as the GraphQL `errors` check has always read it: null,
/// false, zero and empty values are no errors.
fn is_truthy(value: &Value) -> bool {
    !(value.is_null()
        || value.as_bool() == Some(false)
        || value.as_f64() == Some(0.0)
        || value.as_str().is_some_and(str::is_empty)
        || value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(Map::is_empty))
}

fn error_message(error: &Value) -> String {
    if error.is_object() {
        error["message"]
            .as_str()
            .unwrap_or("unknown error")
            .to_owned()
    } else {
        error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answering(stdout: &str) -> impl Runner {
        let stdout = stdout.to_owned();
        move |_: &[String]| {
            Ok(Output {
                success: true,
                stdout: stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    fn envelope(stdout: &str) -> Result<Map<String, Value>, GhError> {
        graphql(&answering(stdout), "Search 'author'", &[])
    }

    #[test]
    fn data_block_is_returned() {
        let data = envelope(r#"{"data": {"viewer": {"login": "me"}}}"#).unwrap();
        assert_eq!(data["viewer"]["login"], "me");
    }

    #[test]
    fn nonzero_exit_carries_the_context_and_detail() {
        let failing = |_: &[String]| {
            Ok(Output {
                success: false,
                stdout: String::new(),
                stderr: "401\n".to_owned(),
            })
        };
        assert_eq!(
            graphql(&failing, "Search 'author'", &[]),
            Err(GhError("Search 'author' failed: 401".to_owned()))
        );
    }

    #[test]
    fn detail_falls_back_to_stdout() {
        let failing = |_: &[String]| {
            Ok(Output {
                success: false,
                stdout: " oops ".to_owned(),
                stderr: " ".to_owned(),
            })
        };
        assert_eq!(graphql(&failing, "X", &[]).unwrap_err().0, "X failed: oops");
    }

    #[test]
    fn invalid_json_raises() {
        assert!(
            envelope("not json")
                .unwrap_err()
                .0
                .starts_with("Search 'author': invalid JSON from gh: ")
        );
    }

    #[test]
    fn non_object_payload_raises() {
        assert_eq!(
            envelope("null").unwrap_err().0,
            "Search 'author': unexpected response from gh (not a JSON object)"
        );
    }

    #[test]
    fn graphql_errors_array_raises() {
        let error = envelope(
            r#"{"data": null, "errors": [{"message": "rate limited"}, {"type": "X"}, "boom"]}"#,
        );
        assert_eq!(
            error.unwrap_err().0,
            "Search 'author' returned errors: rate limited; unknown error; boom"
        );
    }

    #[test]
    fn empty_errors_array_is_no_error() {
        assert!(envelope(r#"{"data": {}, "errors": []}"#).is_ok());
    }

    #[test]
    fn missing_data_block_raises() {
        for payload in [r#"{"data": null}"#, "{}", r#"{"data": []}"#] {
            assert_eq!(
                envelope(payload).unwrap_err().0,
                "Search 'author': response has no data block (unexpected shape)"
            );
        }
    }

    #[test]
    fn args_go_after_api_graphql() {
        let seen = std::sync::Mutex::new(Vec::new());
        let recorder = |args: &[String]| {
            seen.lock().unwrap().push(args.to_vec());
            Ok(Output {
                success: true,
                stdout: r#"{"data": {}}"#.to_owned(),
                stderr: String::new(),
            })
        };
        graphql(&recorder, "X", &["-f".to_owned(), "q=x".to_owned()]).unwrap();
        assert_eq!(
            seen.into_inner().unwrap(),
            [["api", "graphql", "-f", "q=x"]]
        );
    }
}
