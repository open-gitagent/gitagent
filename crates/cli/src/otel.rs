//! OpenTelemetry export, off unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set (and
//! never in `--bench`). After a run, the trajectory and counters become OTLP
//! traces (a run span, a span per turn and per tool call) and metrics (the
//! counters), posted as OTLP/HTTP JSON.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use gitagent_core::RunReport;
use serde_json::{json, Value};

pub(crate) struct Exporter {
    endpoint: String,
    headers: Vec<(String, String)>,
    service: String,
}

impl Exporter {
    pub fn from_env() -> Option<Exporter> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|e| !e.trim().is_empty())?;
        let headers = std::env::var("OTEL_EXPORTER_OTLP_HEADERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Some(Exporter {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            headers,
            service: std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "gitagent".into()),
        })
    }

    pub async fn export(&self, report: &RunReport, model: &str) -> Result<()> {
        let trajectory: Value = serde_json::from_slice(
            &std::fs::read(&report.trajectory_path).context("reading the trajectory")?,
        )?;
        let traces = traces(&trajectory, report, model, &self.service, now_nanos());
        let metrics = metrics(report, &self.service, now_nanos());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        for (path, body) in [("v1/traces", traces), ("v1/metrics", metrics)] {
            let mut req = client
                .post(format!("{}/{path}", self.endpoint))
                .header("content-type", "application/json")
                .body(body.to_string());
            for (k, v) in &self.headers {
                req = req.header(k, v);
            }
            let resp = req.send().await?;
            if !resp.status().is_success() {
                bail!("{path}: HTTP {}", resp.status());
            }
        }
        Ok(())
    }
}

fn resource(service: &str) -> Value {
    json!({"attributes": [
        attr("service.name", json!({"stringValue": service})),
        attr("service.version", json!({"stringValue": env!("CARGO_PKG_VERSION")})),
    ]})
}

fn attr(key: &str, value: Value) -> Value {
    json!({"key": key, "value": value})
}

fn int(v: u64) -> Value {
    json!({"intValue": v.to_string()})
}

fn text(v: &str) -> Value {
    json!({"stringValue": v})
}

pub(crate) fn traces(t: &Value, report: &RunReport, model: &str, service: &str, now: u64) -> Value {
    let trace_id = hex_id(16, now);
    let root_id = hex_id(8, now ^ 1);
    let steps = t["steps"].as_array().cloned().unwrap_or_default();
    let times: Vec<u64> = steps
        .iter()
        .map(|s| {
            s["timestamp"]
                .as_str()
                .and_then(parse_rfc3339)
                .unwrap_or(now)
        })
        .collect();
    let start = times.first().copied().unwrap_or(now);
    let mut spans = vec![json!({
        "traceId": trace_id,
        "spanId": root_id,
        "name": "gitagent.run",
        "kind": 1,
        "startTimeUnixNano": start.to_string(),
        "endTimeUnixNano": now.to_string(),
        "attributes": [
            attr("gen_ai.request.model", text(model)),
            attr("gitagent.outcome", text(&report.outcome.to_string())),
            attr("gitagent.turns", int(report.turns.into())),
            attr("gen_ai.usage.input_tokens", int(report.usage.input_tokens)),
            attr("gen_ai.usage.output_tokens", int(report.usage.output_tokens)),
            attr("gitagent.usage.cache_read_tokens", int(report.usage.cache_read_tokens)),
        ],
    })];
    let mut turn = 0u64;
    for (i, step) in steps.iter().enumerate() {
        if step["source"] != "agent" {
            continue;
        }
        turn += 1;
        // A step runs from the previous step's timestamp to its own.
        let begin = if i > 0 { times[i - 1] } else { start };
        let end = times[i].max(begin);
        let turn_id = hex_id(8, now ^ (turn << 8));
        let m = &step["metrics"];
        spans.push(json!({
            "traceId": trace_id,
            "spanId": turn_id,
            "parentSpanId": root_id,
            "name": "gitagent.turn",
            "kind": 1,
            "startTimeUnixNano": begin.to_string(),
            "endTimeUnixNano": end.to_string(),
            "attributes": [
                attr("gitagent.turn", int(turn)),
                attr("gen_ai.usage.input_tokens", int(m["prompt_tokens"].as_u64().unwrap_or(0))),
                attr("gen_ai.usage.output_tokens", int(m["completion_tokens"].as_u64().unwrap_or(0))),
                attr("gitagent.usage.cache_read_tokens", int(m["cached_tokens"].as_u64().unwrap_or(0))),
            ],
        }));
        for (k, call) in step["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            spans.push(json!({
                "traceId": trace_id,
                "spanId": hex_id(8, now ^ (turn << 8) ^ (k as u64 + 1) << 32),
                "parentSpanId": turn_id,
                "name": "gitagent.tool",
                "kind": 1,
                "startTimeUnixNano": begin.to_string(),
                "endTimeUnixNano": end.to_string(),
                "attributes": [attr("gen_ai.tool.name", text(call["function_name"].as_str().unwrap_or("")))],
            }));
        }
    }
    json!({"resourceSpans": [{
        "resource": resource(service),
        "scopeSpans": [{"scope": {"name": "gitagent"}, "spans": spans}],
    }]})
}

pub(crate) fn metrics(report: &RunReport, service: &str, now: u64) -> Value {
    let mut out = Vec::new();
    if let Value::Object(counters) = report.counters.to_json() {
        for (name, value) in counters {
            let points: Vec<Value> = match &value {
                Value::Number(n) => vec![json!({
                    "asInt": n.as_u64().unwrap_or(0).to_string(),
                    "timeUnixNano": now.to_string(),
                })],
                Value::Object(labels) => labels
                    .iter()
                    .map(|(label, v)| {
                        json!({
                            "asInt": v.as_u64().unwrap_or(0).to_string(),
                            "timeUnixNano": now.to_string(),
                            "attributes": [attr("name", text(label))],
                        })
                    })
                    .collect(),
                _ => continue,
            };
            out.push(json!({
                "name": format!("gitagent.{name}"),
                "sum": {"dataPoints": points, "aggregationTemporality": 2, "isMonotonic": true},
            }));
        }
    }
    json!({"resourceMetrics": [{
        "resource": resource(service),
        "scopeMetrics": [{"scope": {"name": "gitagent"}, "metrics": out}],
    }]})
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// A hex id of `bytes` length derived from `seed` (unique enough per run).
fn hex_id(bytes: usize, seed: u64) -> String {
    let mut x = seed ^ 0x9e37_79b9_7f4a_7c15 ^ u64::from(std::process::id());
    let mut out = String::new();
    while out.len() < bytes * 2 {
        // splitmix64
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.push_str(&format!("{:016x}", z ^ (z >> 31)));
    }
    out.truncate(bytes * 2);
    out
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` into Unix nanoseconds.
pub(crate) fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>());
    let (hh, mm, ss) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    // days_from_civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    let nanos: i64 = format!("{frac:0<9}")[..9].parse().ok()?;
    u64::try_from(secs * 1_000_000_000 + nanos).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitagent_core::Outcome;
    use gitagent_provider::Usage;
    use gitagent_record::Counters;

    fn report() -> RunReport {
        let mut counters = Counters::new();
        counters.add("turns", 2);
        counters.inc_labelled("tool_calls", "exec");
        RunReport {
            outcome: Outcome::Completed,
            turns: 2,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_tokens: 60,
                cache_write_tokens: 0,
            },
            counters,
            trajectory_path: "t.json".into(),
            summary: None,
        }
    }

    #[test]
    fn parses_our_timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-10-14T13:54:56.789Z"),
            Some(1_791_986_096_789_000_000)
        );
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    #[test]
    fn builds_spans_and_metrics() {
        let t = json!({"steps": [
            {"source": "system", "timestamp": "2026-10-14T10:00:00.000Z"},
            {"source": "user", "timestamp": "2026-10-14T10:00:00.100Z"},
            {"source": "agent", "timestamp": "2026-10-14T10:00:02.000Z",
             "metrics": {"prompt_tokens": 50, "completion_tokens": 10, "cached_tokens": 0},
             "tool_calls": [{"function_name": "exec"}]},
            {"source": "agent", "timestamp": "2026-10-14T10:00:03.000Z",
             "metrics": {"prompt_tokens": 50, "completion_tokens": 10, "cached_tokens": 60},
             "tool_calls": [{"function_name": "task_complete"}]}
        ]});
        let tr = traces(
            &t,
            &report(),
            "anthropic:m",
            "svc",
            1_792_000_000_000_000_000,
        );
        let spans = tr["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = spans.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "gitagent.run",
                "gitagent.turn",
                "gitagent.tool",
                "gitagent.turn",
                "gitagent.tool"
            ]
        );
        assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
        assert_eq!(spans[2]["parentSpanId"], spans[1]["spanId"]);
        assert_eq!(spans[0]["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(spans[1]["startTimeUnixNano"], "1791972000100000000");
        let ids: std::collections::HashSet<&str> = spans
            .iter()
            .map(|s| s["spanId"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), spans.len());

        let m = metrics(&report(), "svc", 5);
        let list = m["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .unwrap();
        let tool = list
            .iter()
            .find(|x| x["name"] == "gitagent.tool_calls")
            .unwrap();
        assert_eq!(tool["sum"]["dataPoints"][0]["asInt"], "1");
        assert_eq!(
            tool["sum"]["dataPoints"][0]["attributes"][0]["value"]["stringValue"],
            "exec"
        );
    }
}
