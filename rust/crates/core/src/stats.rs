//! Statistiques du processus + outil `gh_stats` (lecture seule, export JSON local optionnel).

use crate::error::{GhError, Result};
use crate::tool::Tool;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct PerTool {
    calls: u64,
    errors: u64,
    test_calls: u64,
    real_calls: u64,
    latencies_ms: Vec<u64>, // les 1000 dernières
    last_call: Option<String>,
    last_error: Option<String>,
}

#[derive(Default)]
struct Inner {
    tools: BTreeMap<String, PerTool>,
    egress_blocked: u64,
    approved: u64,
    denied: u64,
    timeouts: u64,
    confirmation_required: u64,
}

#[derive(Default)]
pub struct Stats(Mutex<Inner>);

pub fn global() -> &'static Stats {
    static S: OnceLock<Stats> = OnceLock::new();
    S.get_or_init(Stats::default)
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

impl Stats {
    /// Appelé par le serveur autour de chaque tools/call.
    pub fn record(&self, tool: &str, err: Option<&GhError>, ms: u64) {
        let mut g = self.0.lock().unwrap();
        let t = g.tools.entry(tool.into()).or_default();
        t.calls += 1;
        if crate::net::is_test() {
            t.test_calls += 1;
        } else {
            t.real_calls += 1;
        }
        if let Some(e) = err {
            t.errors += 1;
            t.last_error = Some(e.code().into());
        }
        if t.latencies_ms.len() >= 1000 {
            t.latencies_ms.remove(0);
        }
        t.latencies_ms.push(ms);
        t.last_call = Some(chrono::Utc::now().to_rfc3339());
    }

    pub fn egress_blocked(&self) {
        self.0.lock().unwrap().egress_blocked += 1;
    }

    pub fn approval(&self, r: &Result<()>) {
        let mut g = self.0.lock().unwrap();
        match r {
            Ok(()) => g.approved += 1,
            Err(GhError::Denied(_)) => g.denied += 1,
            Err(GhError::Timeout(_)) => g.timeouts += 1,
            Err(_) => g.confirmation_required += 1,
        }
    }

    pub fn snapshot(&self) -> Value {
        let g = self.0.lock().unwrap();
        let tools: serde_json::Map<String, Value> = g
            .tools
            .iter()
            .map(|(k, t)| {
                let mut l = t.latencies_ms.clone();
                l.sort_unstable();
                (
                    k.clone(),
                    json!({
                        "calls": t.calls, "errors": t.errors, "real_calls": t.real_calls, "test_calls": t.test_calls,
                        "p50_ms": pct(&l, 0.5), "p95_ms": pct(&l, 0.95), "last_call": t.last_call, "last_error": t.last_error,
                    }),
                )
            })
            .collect();
        let sum = |f: fn(&PerTool) -> u64| g.tools.values().map(f).sum::<u64>();
        json!({
            "test_mode": crate::net::is_test(),
            "totals": { "calls": sum(|t| t.calls), "errors": sum(|t| t.errors), "real_calls": sum(|t| t.real_calls), "test_calls": sum(|t| t.test_calls) },
            "egress_blocked": g.egress_blocked,
            "approvals": { "approved": g.approved, "denied": g.denied, "timeouts": g.timeouts, "confirmation_required": g.confirmation_required },
            "tools": tools,
        })
    }
}

/// Outil `gh_stats`.
pub fn tool() -> Tool {
    Tool::new(
        "gh_stats",
        "Server statistics: calls per tool, errors, p50/p95 latency, last call, real vs test-mode calls, blocked egress, approvals/denials/timeouts. Read-only. Optional export_path writes the same JSON to a local file.",
        json!({ "type": "object", "properties": { "export_path": { "type": "string", "description": "Local file path to write the JSON snapshot to." } } }),
        |_ctx, args| async move {
            let snap = global().snapshot();
            if let Some(p) = args.get("export_path").and_then(Value::as_str) {
                std::fs::write(p, serde_json::to_string_pretty(&snap).unwrap_or_default()).map_err(|e| GhError::Other(format!("export: {e}")))?;
            }
            Ok(snap)
        },
    )
}
