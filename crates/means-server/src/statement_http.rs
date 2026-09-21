//! Shared transport checks for read-only statement clients.
use anyhow::{bail, Context, Result};
use serde_json::Value;
pub fn origin(api: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(api).context("invalid bank API origin")?;
    let local = u.host_str().is_some_and(|h| h == "localhost" || h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    if !(u.scheme() == "https" || u.scheme() == "http" && local) || !u.username().is_empty() || u.password().is_some() || u.path() != "/" || u.query().is_some() || u.fragment().is_some() {
        bail!("bank API requires an HTTPS origin; HTTP is allowed only on loopback")
    }
    Ok(u)
}
pub fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(90))
}
pub async fn response(request: reqwest::RequestBuilder, provider: &str) -> Result<Value> {
    let r = request.send().await.map_err(|e| e.without_url()).context("reach bank API")?;
    if !r.status().is_success() {
        if provider == "Wise" && r.status() == reqwest::StatusCode::FORBIDDEN {
            bail!("Wise denied statement access (403); check token permissions, profile region and SCA requirements")
        }
        bail!("{provider} returned HTTP {}; no incomplete account will be published", r.status())
    }
    let bytes = r.bytes().await.map_err(|e| e.without_url()).context("read bank response")?;
    serde_json::from_slice(&bytes).context("invalid bank JSON response")
}
pub fn ranges(from: chrono::NaiveDate, to: chrono::NaiveDate, days: i64) -> Result<Vec<(chrono::NaiveDate, chrono::NaiveDate)>> {
    if from > to || days < 1 {
        bail!("booking start must not be after the end date")
    }
    let mut ranges = Vec::new();
    let mut start = from;
    loop {
        let end = start.checked_add_signed(chrono::Duration::days(days - 1)).context("date range overflow")?.min(to);
        ranges.push((start, end));
        if ranges.len() > 1000 {
            bail!("too many statement intervals")
        }
        if end == to {
            break;
        }
        start = end.succ_opt().context("date range overflow")?;
    }
    Ok(ranges)
}
