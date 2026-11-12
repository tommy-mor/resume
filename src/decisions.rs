//! OpenRouter Decisions API client — the only AI path.
//!
//! The Python builder had three providers (Anthropic, OpenAI, OpenRouter),
//! LLM-prose ranking, and a local keyword fallback. All of that is gone.
//! Every judgment in the sorter goes through one surface:
//! `POST https://openrouter.ai/api/alpha/decisions` with a single
//! `choice` question ("which entry is more relevant?") per pairwise
//! comparison, answered by `google/gemini-decision-1` (JEV-like decision
//! model: typed probabilities, no free text). The API key comes from
//! `OPENROUTER_KEY` / `OPENROUTER_API_KEY` in the environment or `.env`.
//! Any failure — missing key, HTTP error, refusal, malformed answer —
//! aborts the build. There is no local fallback to score with.
//!
//! Request shape (one question per call):
//! ```json
//! {
//!   "model": "google/gemini-decision-1",
//!   "state": { "job_description": "...", "item_a": "...", "item_b": "..." },
//!   "questions": {
//!     "better": {
//!       "type": "choice",
//!       "instructions": "Which resume entry is more relevant ...?",
//!       "criteria": { "a": "...", "b": "..." }
//!     }
//!   }
//! }
//! ```
//!
//! The response's `answers.better` carries `choice` (`"a"`/`"b"`),
//! `probabilities`, and `confidence`. Probabilities are clamped to
//! `[0.01, 0.99]` so the loser still gets a nonzero edge weight (a zero
//! weight would disconnect that pair's flow), nudged so the chosen side
//! is strictly ahead (a `0.5/0.5` tie names no winner), then renormalized
//! to sum to 1. Those two numbers are the vote's `(a_share, b_share)` —
//! the same scalar-ratio shape muse-tricks records.

use std::time::Duration;

const ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
/// Default decision model. Overridable with `--model`, but always served
/// by OpenRouter through the Decisions API.
pub const DEFAULT_MODEL: &str = "google/gemini-decision-1";

pub struct DecisionsClient {
    client: reqwest::blocking::Client,
    key: String,
    model: String,
}

impl DecisionsClient {
    /// Build from the environment (after `.env` is loaded). Fails hard
    /// when no OpenRouter key is present.
    pub fn from_env(model: &str) -> Result<Self, String> {
        // `.env` in the current working directory; missing file is fine,
        // the real environment still wins either way.
        let _ = dotenvy::dotenv();
        let key = std::env::var("OPENROUTER_KEY")
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                "OPENROUTER_KEY is not set (environment or .env); the sorter has no \
                 fallback without it"
                    .to_string()
            })?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("build HTTP client: {e}"))?;
        Ok(Self {
            client,
            key,
            model: model.to_string(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Judge one pair. `a_text` / `b_text` are the `describe()` blobs.
    /// Returns `(a_share, b_share)` — both in `(0, 1)`, summing to 1,
    /// with the chosen option strictly greater.
    pub fn decide_pair(
        &self,
        job_description: &str,
        a_text: &str,
        b_text: &str,
    ) -> Result<(f64, f64), String> {
        let body = serde_json::json!({
            "model": self.model,
            "state": {
                "job_description": job_description,
                "item_a": a_text,
                "item_b": b_text,
            },
            "questions": {
                "better": {
                    "type": "choice",
                    "instructions": "Which of the two resume entries is more relevant \
                                     to the job description? Prefer specific skill, tool, \
                                     and domain overlap over generic seniority.",
                    "criteria": {
                        "a": "Entry A (item_a) below is more relevant to the job description.",
                        "b": "Entry B (item_b) below is more relevant to the job description."
                    }
                }
            }
        });
        let resp = self
            .client
            .post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/resume-builder")
            .header("X-Title", "resume pairwise sorter")
            .json(&body)
            .send()
            .map_err(|e| {
                use std::error::Error as _;
                let mut causes = Vec::new();
                let mut cur = e.source();
                while let Some(s) = cur {
                    causes.push(s.to_string());
                    cur = s.source();
                }
                let cause = if causes.is_empty() {
                    String::new()
                } else {
                    format!(" (cause: {})", causes.join("; "))
                };
                format!(
                    "decisions request failed (no retry) — POST {ENDPOINT}: {e}{cause}; \
                     check network/firewall/proxy and that OPENROUTER_KEY is set"
                )
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            // Status + body for debugging, never the key.
            return Err(format!(
                "decisions API HTTP {status}: {}",
                truncate(&text, 500)
            ));
        }
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| format!("decisions response is not JSON: {e}"))?;
        let answer = v
            .get("answers")
            .and_then(|a| a.get("better"))
            .ok_or_else(|| format!("decisions response missing answers.better: {v}"))?;
        let ty = answer
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if ty == "refusal" {
            return Err("decisions model refused to answer this pair".to_string());
        }
        if ty != "choice" {
            return Err(format!("decisions answer type is {ty:?}, expected choice: {answer}"));
        }
        let choice = answer
            .get("choice")
            .and_then(|c| c.as_str())
            .ok_or_else(|| format!("decisions answer missing choice: {answer}"))?;
        let probs = answer
            .get("probabilities")
            .and_then(|p| p.as_object())
            .ok_or_else(|| format!("decisions answer missing probabilities: {answer}"))?;
        let pa = probs
            .get("a")
            .and_then(|p| p.as_f64())
            .ok_or_else(|| format!("decisions probabilities missing a: {answer}"))?;
        let pb = probs
            .get("b")
            .and_then(|p| p.as_f64())
            .ok_or_else(|| format!("decisions probabilities missing b: {answer}"))?;
        if !pa.is_finite() || !pb.is_finite() {
            return Err(format!("decisions probabilities not finite: a={pa} b={pb}"));
        }
        match choice {
            "a" | "b" => {}
            other => return Err(format!("decisions choice is {other:?}, expected a/b")),
        }
        Ok(clamp_and_prefer(pa, pb, choice))
    }
}

/// Clamp to `[0.01, 0.99]`, ensure the chosen side is strictly ahead
/// (ties name no winner), renormalize to sum 1.
fn clamp_and_prefer(pa: f64, pb: f64, choice: &str) -> (f64, f64) {
    let mut a = pa.clamp(0.01, 0.99);
    let mut b = pb.clamp(0.01, 0.99);
    if choice == "a" && a <= b {
        a = (b + 0.02).min(0.99);
    } else if choice == "b" && b <= a {
        b = (a + 0.02).min(0.99);
    }
    let sum = a + b;
    (a / sum, b / sum)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_prefers_chosen_side_on_tie() {
        let (a, b) = clamp_and_prefer(0.5, 0.5, "a");
        assert!(a > b, "a={a} b={b}");
        assert!((a + b - 1.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_never_zero() {
        let (a, b) = clamp_and_prefer(0.0, 1.0, "b");
        assert!(a > 0.0 && b > 0.0, "a={a} b={b}");
        assert!((a + b - 1.0).abs() < 1e-9);
    }
}
