//! Price sources. Each party picks its own: nobody has to trust a common oracle.
//!
//! The parsers are pure functions, tested on sample answers. The network calls are thin wrappers.

use anyhow::{anyhow, Context, Result};
use menger_core::{Asset, Quote};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

const USER_AGENT: &str = "menger-ln/0.1 (+https://menger.money)";

/// HTTP client. Honours HTTPS_PROXY / ALL_PROXY, including `socks5://127.0.0.1:9050` to read the
/// prices through Tor, and the system's certificates.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
}

pub trait Source: Send {
    fn name(&self) -> &str;
    /// Reads the source now. `now` is used when the source gives no time of its own.
    fn fetch(&self, now: i64) -> Result<Vec<Quote>>;
}

/// Reads all sources; a failing source is reported and skipped, it never stops the others.
pub fn read_all(sources: &[Box<dyn Source>], now: i64) -> (Vec<Quote>, Vec<String>) {
    let mut quotes = Vec::new();
    let mut errors = Vec::new();
    for s in sources {
        match s.fetch(now) {
            Ok(q) => quotes.extend(q),
            Err(e) => errors.push(format!("{}: {e:#}", s.name())),
        }
    }
    (quotes, errors)
}

/// The usual set: three bitcoin markets, and two gold references that must agree within 1 %: the
/// spot price from Swissquote and the PAXG token on Kraken (one PAXG is one troy ounce). Only the
/// price of PAXG is read; nothing is held. Gold quotes are ignored on weekends, when the last
/// Friday price is kept, so the token's weekend trading never counts.
pub fn live_sources() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(Http::new("kraken", Asset::Btc, "https://api.kraken.com/0/public/Ticker?pair=XBTUSD", parse_kraken)),
        Box::new(Http::new("bitstamp", Asset::Btc, "https://www.bitstamp.net/api/v2/ticker/btcusd/", parse_bitstamp)),
        Box::new(Http::new("coinbase", Asset::Btc, "https://api.coinbase.com/v2/prices/BTC-USD/spot", parse_coinbase)),
        Box::new(Http::new(
            "swissquote",
            Asset::Gold,
            "https://forex-data-feed.swissquote.com/public-quotes/bboquotes/instrument/XAU/USD",
            parse_swissquote,
        )),
        Box::new(Http::new("kraken-paxg", Asset::Gold, "https://api.kraken.com/0/public/Ticker?pair=PAXGUSD", parse_kraken)),
    ]
}

type Parser = fn(&Value, i64) -> Result<(f64, i64)>;

pub struct Http {
    name: String,
    asset: Asset,
    url: String,
    parse: Parser,
}

impl Http {
    pub fn new(name: &str, asset: Asset, url: &str, parse: Parser) -> Self {
        Http { name: name.into(), asset, url: url.into(), parse }
    }
}

impl Source for Http {
    fn name(&self) -> &str {
        &self.name
    }

    fn fetch(&self, now: i64) -> Result<Vec<Quote>> {
        let body: Value = agent()
            .get(&self.url)
            .call()
            .with_context(|| format!("request to {}", self.url))?
            .into_json()
            .context("answer is not JSON")?;
        let (usd, at) = (self.parse)(&body, now)?;
        Ok(vec![Quote { source: self.name.clone(), asset: self.asset, usd, at }])
    }
}

/// A local file `{"btc_usd": 100000, "xau_usd": 4319}`, for demonstrations and tests. Each value
/// counts as one source named `fichier`.
pub struct File {
    pub path: PathBuf,
}

impl Source for File {
    fn name(&self) -> &str {
        "fichier"
    }

    fn fetch(&self, now: i64) -> Result<Vec<Quote>> {
        let text = std::fs::read_to_string(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        let v: Value = serde_json::from_str(&text).context("price file is not JSON")?;
        let mut out = Vec::new();
        for (key, asset) in [("btc_usd", Asset::Btc), ("xau_usd", Asset::Gold)] {
            if let Some(usd) = v.get(key).and_then(Value::as_f64) {
                out.push(Quote { source: "fichier".into(), asset, usd, at: now });
            }
        }
        Ok(out)
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn positive(usd: Option<f64>, what: &str) -> Result<f64> {
    match usd {
        Some(x) if x.is_finite() && x > 0.0 => Ok(x),
        _ => Err(anyhow!("no valid {what} in answer")),
    }
}

/// `{"error":[],"result":{"XXBTZUSD":{"c":["67000.1","0.01"], ...}}}`: last trade price.
pub fn parse_kraken(v: &Value, now: i64) -> Result<(f64, i64)> {
    if let Some(errs) = v.get("error").and_then(Value::as_array) {
        if !errs.is_empty() {
            return Err(anyhow!("kraken error: {errs:?}"));
        }
    }
    let pair = v.get("result").and_then(Value::as_object).and_then(|m| m.values().next());
    let usd = pair.and_then(|p| p.get("c")).and_then(|c| c.get(0)).and_then(number);
    Ok((positive(usd, "price")?, now))
}

/// `{"last":"67000","timestamp":"1730000000", ...}`.
pub fn parse_bitstamp(v: &Value, now: i64) -> Result<(f64, i64)> {
    let usd = v.get("last").and_then(number);
    let at = v.get("timestamp").and_then(number).map(|t| t as i64).unwrap_or(now);
    Ok((positive(usd, "price")?, at))
}

/// `{"data":{"amount":"67000.12","base":"BTC","currency":"USD"}}`.
pub fn parse_coinbase(v: &Value, now: i64) -> Result<(f64, i64)> {
    let data = v.get("data");
    let pair_ok = data.and_then(|d| d.get("base")).and_then(Value::as_str) == Some("BTC")
        && data.and_then(|d| d.get("currency")).and_then(Value::as_str) == Some("USD");
    if !pair_ok {
        return Err(anyhow!("coinbase answer is not BTC-USD"));
    }
    let usd = data.and_then(|d| d.get("amount")).and_then(number);
    Ok((positive(usd, "price")?, now))
}

/// A list of platforms, each with `spreadProfilePrices` (bid/ask) and `ts` in milliseconds.
/// Uses the median of the mid prices.
pub fn parse_swissquote(v: &Value, now: i64) -> Result<(f64, i64)> {
    let mut mids = Vec::new();
    let mut at = None;
    for platform in v.as_array().into_iter().flatten() {
        for p in platform.get("spreadProfilePrices").and_then(Value::as_array).into_iter().flatten() {
            if let (Some(b), Some(a)) = (p.get("bid").and_then(number), p.get("ask").and_then(number)) {
                if b > 0.0 && a >= b {
                    mids.push((a + b) / 2.0);
                }
            }
        }
        if let Some(ts) = platform.get("ts").and_then(number) {
            let t = (ts / 1000.0) as i64;
            at = Some(at.map_or(t, |x: i64| x.max(t)));
        }
    }
    if mids.is_empty() {
        return Err(anyhow!("no bid/ask in swissquote answer"));
    }
    mids.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok((positive(Some(mids[mids.len() / 2]), "price")?, at.unwrap_or(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_793_793_600;

    #[test]
    fn kraken() {
        let v = json!({"error": [], "result": {"XXBTZUSD": {"a": ["101.0","1","1.0"], "c": ["100250.5", "0.01"]}}});
        assert_eq!(parse_kraken(&v, NOW).unwrap(), (100_250.5, NOW));
        assert!(parse_kraken(&json!({"error": ["EQuery:Unknown asset pair"], "result": {}}), NOW).is_err());
    }

    #[test]
    fn bitstamp() {
        let v = json!({"last": "100100", "timestamp": "1793793590", "bid": "100099"});
        assert_eq!(parse_bitstamp(&v, NOW).unwrap(), (100_100.0, 1_793_793_590));
    }

    #[test]
    fn coinbase() {
        let v = json!({"data": {"amount": "99990.12", "base": "BTC", "currency": "USD"}});
        assert_eq!(parse_coinbase(&v, NOW).unwrap(), (99_990.12, NOW));
        assert!(parse_coinbase(&json!({"data": {"amount": "1", "base": "ETH", "currency": "USD"}}), NOW).is_err());
    }

    #[test]
    fn swissquote() {
        let v = json!([
            {"topo": {"platform": "SwissquoteLtd"}, "spreadProfilePrices": [
                {"spreadProfile": "prime", "bid": 4318.0, "ask": 4320.0},
                {"spreadProfile": "standard", "bid": 4317.0, "ask": 4321.0}
            ], "ts": 1_793_793_595_000i64},
            {"topo": {"platform": "AT"}, "spreadProfilePrices": [
                {"spreadProfile": "prime", "bid": 4318.5, "ask": 4319.5}
            ], "ts": 1_793_793_598_000i64}
        ]);
        assert_eq!(parse_swissquote(&v, NOW).unwrap(), (4_319.0, 1_793_793_598));
        assert!(parse_swissquote(&json!([]), NOW).is_err());
    }

    #[test]
    fn file_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prix.json");
        std::fs::write(&path, r#"{"btc_usd": 90000, "xau_usd": 4319}"#).unwrap();
        let q = File { path }.fetch(NOW).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].asset, Asset::Btc);
        assert_eq!(q[1].usd, 4_319.0);
    }
}
