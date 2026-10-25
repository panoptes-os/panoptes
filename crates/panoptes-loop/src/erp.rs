//! The verifier: after an agent's run, ask ERPNext's REST API whether its
//! invoice really went in, with the same checks as the demo's verify.sh.
//!
//! ```text
//! POST /api/method/login                      -> sid cookie, once per run
//! GET  /api/resource/Purchase Invoice?filters=[supplier, bill_no, docstatus=1]
//! GET  /api/resource/Purchase Invoice/{name}  -> date, po, lines, tax, total
//! GET  /api/resource/File?filters=[attached_to = that invoice]
//! ```

use serde_json::{Value, json};

pub struct Erp {
    http: reqwest::Client,
    base: String,
    sid: String,
}

impl Erp {
    pub async fn login(base: &str, user: &str, password: &str) -> Result<Self, String> {
        let http = reqwest::Client::new();
        let base = base.trim_end_matches('/').to_owned();
        let r = http
            .post(format!("{base}/api/method/login"))
            .json(&json!({ "usr": user, "pwd": password }))
            .send()
            .await
            .map_err(|e| format!("erp login: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("erp login: HTTP {}", r.status()));
        }
        let sid = r
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.strip_prefix("sid=").map(|r| r.split(';').next().unwrap_or("").to_owned()))
            .ok_or("erp login: no sid cookie")?;
        Ok(Erp { http, base, sid })
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, String> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base)).map_err(|e| e.to_string())?;
        for (k, v) in query {
            url.query_pairs_mut().append_pair(k, v);
        }
        let r = self
            .http
            .get(url)
            .header(reqwest::header::COOKIE, format!("sid={}", self.sid))
            .send()
            .await
            .map_err(|e| format!("{path}: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("{path}: HTTP {}", r.status()));
        }
        r.json().await.map_err(|e| format!("{path}: {e}"))
    }

    /// `{verified, attached, pi, failed}` for one truth row. `failed` names
    /// the checks that did not hold.
    pub async fn check(&self, truth: &Value) -> Value {
        match self.check_inner(truth).await {
            Ok(v) => v,
            Err(e) => json!({ "verified": false, "attached": false, "failed": ["query"], "verify_error": e }),
        }
    }

    async fn check_inner(&self, t: &Value) -> Result<Value, String> {
        let filters = json!([["supplier", "=", t["supplier"]], ["bill_no", "=", t["invoice_no"]], ["docstatus", "=", 1]]);
        let list = self
            .get(
                "/api/resource/Purchase%20Invoice",
                &[("filters", filters.to_string()), ("fields", r#"["name"]"#.into()), ("limit_page_length", "0".into())],
            )
            .await?;
        let names: Vec<&str> = list["data"].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str()).collect();
        let Some(name) = names.first() else {
            return Ok(json!({ "verified": false, "attached": false, "failed": ["submitted"] }));
        };
        let doc = self.get(&format!("/api/resource/Purchase%20Invoice/{}", enc(name)), &[]).await?;
        let failed = mismatches(&doc["data"], t);
        let files = json!([["attached_to_doctype", "=", "Purchase Invoice"], ["attached_to_name", "=", name]]);
        let files = self
            .get("/api/resource/File", &[("filters", files.to_string()), ("fields", r#"["file_name"]"#.into())])
            .await?;
        let attached: Vec<&str> = files["data"].as_array().into_iter().flatten().filter_map(|f| f["file_name"].as_str()).collect();
        Ok(json!({ "verified": failed.is_empty(), "attached": !attached.is_empty(), "pi": name,
                   "submitted_count": names.len(), "files": attached, "failed": failed }))
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// verify.sh's checks, by name: date, po, lines (item, qty, rate), tax,
/// total. The document is already known submitted.
pub fn mismatches(doc: &Value, t: &Value) -> Vec<&'static str> {
    let near = |a: &Value, b: &Value| match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < 0.006,
        _ => false,
    };
    let lines = |v: &Value| {
        let mut l: Vec<String> = v
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                let n = |k: &str, s: f64| (i[k].as_f64().unwrap_or(f64::NAN) * s).round();
                format!("{}|{}|{}", i["item_code"].as_str().unwrap_or(""), n("qty", 1000.0), n("rate", 100.0))
            })
            .collect();
        l.sort();
        l
    };
    let items = doc["items"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut out = Vec::new();
    if doc["bill_date"] != t["date"] {
        out.push("date");
    }
    if items.is_empty() || !items.iter().all(|i| i["purchase_order"] == t["po"]) {
        out.push("po");
    }
    if lines(&doc["items"]) != lines(&t["lines"]) {
        out.push("lines");
    }
    if !near(&doc["total_taxes_and_charges"], &t["tax"]) {
        out.push("tax");
    }
    if !near(&doc["grand_total"], &t["total"]) {
        out.push("total");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_match_verify_sh() {
        let t = json!({ "date": "2026-01-05", "po": "PO-1", "tax": 10.0, "total": 110.0,
                        "lines": [{ "item_code": "A", "qty": 2, "rate": 50 }] });
        let mut d = json!({ "bill_date": "2026-01-05", "total_taxes_and_charges": 10.001, "grand_total": 110,
                            "items": [{ "item_code": "A", "qty": 2.0, "rate": 50.0, "purchase_order": "PO-1" }] });
        assert!(mismatches(&d, &t).is_empty());
        d["items"][0]["purchase_order"] = json!(null);
        d["grand_total"] = json!(111);
        assert_eq!(mismatches(&d, &t), vec!["po", "total"]);
        assert_eq!(enc("ACC-PINV-2026-00001"), "ACC-PINV-2026-00001");
        assert_eq!(enc("a b"), "a%20b");
    }
}
