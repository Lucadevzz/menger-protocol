//! Price sources. Each party picks its own: nobody has to trust a common oracle.
//!
//! The parsers are pure functions, tested on sample answers. The network calls are thin wrappers.

use anyhow::{anyhow, Context, Result};
use menger_core::{Asset, Quote};
use serde::{Deserialize, Serialize};
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

/// What a source quotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetName {
    Btc,
    Or,
}

/// How a source answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Kraken's `Ticker`: the middle of the best bid and ask.
    Kraken,
    /// Bitstamp's `ticker`: the middle of bid and ask.
    Bitstamp,
    /// Coinbase's `prices/…/spot`.
    Coinbase,
    /// Swissquote's public quotes.
    Swissquote,
    /// Any JSON answer: the price is the number found at `champ`.
    Json,
}

/// One source, as written in a sources file. Each party keeps its own list: nobody has to trust a
/// common oracle, and nobody has to trust this program's choice either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Its name. `family:place` (such as `paxg:kraken`) puts it in a family that counts once.
    pub nom: String,
    pub actif: AssetName,
    pub url: String,
    pub format: Format,
    /// For the `json` format: where the price is, as a JSON pointer such as `/data/price`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub champ: Option<String>,
}

fn spec(nom: &str, actif: AssetName, format: Format, url: &str) -> SourceSpec {
    SourceSpec { nom: nom.into(), actif, url: url.into(), format, champ: None }
}

/// The usual set. Bitcoin: three markets. Gold: three families that must agree within 1 %, the
/// spot price (Swissquote) and two tokens each worth one troy ounce, PAXG (on three markets, which
/// count once) and XAUT. Only prices are read; nothing is held. Gold quotes are ignored on
/// week-ends, when the last Friday price is kept, so the tokens' week-end trading never counts.
pub fn default_specs() -> Vec<SourceSpec> {
    use AssetName::{Btc, Or};
    vec![
        spec("kraken", Btc, Format::Kraken, "https://api.kraken.com/0/public/Ticker?pair=XBTUSD"),
        spec("bitstamp", Btc, Format::Bitstamp, "https://www.bitstamp.net/api/v2/ticker/btcusd/"),
        spec("coinbase", Btc, Format::Coinbase, "https://api.coinbase.com/v2/prices/BTC-USD/spot"),
        spec(
            "spot:swissquote",
            Or,
            Format::Swissquote,
            "https://forex-data-feed.swissquote.com/public-quotes/bboquotes/instrument/XAU/USD",
        ),
        spec("paxg:kraken", Or, Format::Kraken, "https://api.kraken.com/0/public/Ticker?pair=PAXGUSD"),
        spec("paxg:coinbase", Or, Format::Coinbase, "https://api.coinbase.com/v2/prices/PAXG-USD/spot"),
        spec("paxg:bitstamp", Or, Format::Bitstamp, "https://www.bitstamp.net/api/v2/ticker/paxgusd/"),
        spec("xaut:kraken", Or, Format::Kraken, "https://api.kraken.com/0/public/Ticker?pair=XAUTUSD"),
    ]
}

pub fn live_sources() -> Vec<Box<dyn Source>> {
    default_specs().into_iter().map(|s| Box::new(Http { spec: s }) as Box<dyn Source>).collect()
}

/// The sources listed in a file: a JSON array of [`SourceSpec`].
pub fn sources_from_file(path: &std::path::Path) -> Result<Vec<Box<dyn Source>>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    let specs: Vec<SourceSpec> = serde_json::from_str(&text).with_context(|| format!("{} n'est pas une liste de sources", path.display()))?;
    if specs.is_empty() {
        return Err(anyhow!("{} ne contient aucune source", path.display()));
    }
    specs.into_iter().map(|s| Ok(Box::new(Http::new(s)?) as Box<dyn Source>)).collect()
}

pub struct Http {
    spec: SourceSpec,
}

impl Http {
    /// Checks the source before it is used: a price never travels in clear, except from this
    /// computer.
    pub fn new(spec: SourceSpec) -> Result<Self> {
        let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"].iter().any(|p| spec.url.starts_with(p));
        if !(spec.url.starts_with("https://") || local) {
            return Err(anyhow!("source {} : l'adresse doit commencer par https://", spec.nom));
        }
        if spec.nom.trim().is_empty() {
            return Err(anyhow!("une source n'a pas de nom"));
        }
        if spec.format == Format::Json && spec.champ.is_none() {
            return Err(anyhow!("source {} : le format json demande un champ, par exemple /data/price", spec.nom));
        }
        Ok(Http { spec })
    }
}

impl Source for Http {
    fn name(&self) -> &str {
        &self.spec.nom
    }

    fn fetch(&self, now: i64) -> Result<Vec<Quote>> {
        let body: Value = agent()
            .get(&self.spec.url)
            .call()
            .with_context(|| format!("request to {}", self.spec.url))?
            .into_json()
            .context("answer is not JSON")?;
        let (usd, at) = match self.spec.format {
            Format::Kraken => parse_kraken(&body, now)?,
            Format::Bitstamp => parse_bitstamp(&body, now)?,
            Format::Coinbase => parse_coinbase(&body, now, &self.spec.url)?,
            Format::Swissquote => parse_swissquote(&body, now)?,
            Format::Json => parse_pointer(&body, now, self.spec.champ.as_deref().unwrap_or_default())?,
        };
        let asset = match self.spec.actif {
            AssetName::Btc => Asset::Btc,
            AssetName::Or => Asset::Gold,
        };
        Ok(vec![Quote { source: self.spec.nom.clone(), asset, usd, at }])
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

/// The middle of a bid and an ask no more than 1 % apart. The last trade of a quiet market can be
/// hours old; a tight bid and ask are a price someone stands behind right now.
fn middle(bid: Option<f64>, ask: Option<f64>) -> Result<f64> {
    let (bid, ask) = (positive(bid, "bid")?, positive(ask, "ask")?);
    if ask < bid || (ask - bid) / bid > 0.01 {
        return Err(anyhow!("bid {bid} and ask {ask} are too far apart"));
    }
    Ok((bid + ask) / 2.0)
}

/// `{"error":[],"result":{"XXBTZUSD":{"a":["67001","1","1.0"],"b":["67000","1","1.0"], ...}}}`.
pub fn parse_kraken(v: &Value, now: i64) -> Result<(f64, i64)> {
    if let Some(errs) = v.get("error").and_then(Value::as_array) {
        if !errs.is_empty() {
            return Err(anyhow!("kraken error: {errs:?}"));
        }
    }
    let pair = v.get("result").and_then(Value::as_object).filter(|m| m.len() == 1).and_then(|m| m.values().next());
    let side = |key: &str| pair.and_then(|p| p.get(key)).and_then(|x| x.get(0)).and_then(number);
    Ok((middle(side("b"), side("a"))?, now))
}

/// `{"bid":"66999","ask":"67001","last":"67000","timestamp":"1730000000", ...}`.
pub fn parse_bitstamp(v: &Value, now: i64) -> Result<(f64, i64)> {
    let at = v.get("timestamp").and_then(number).map(|t| t as i64).unwrap_or(now);
    let field = |key: &str| v.get(key).and_then(number);
    Ok((middle(field("bid"), field("ask"))?, at))
}

/// `{"data":{"amount":"67000.12","base":"BTC","currency":"USD"}}`. The pair named in the answer
/// must be the one asked for in the address.
pub fn parse_coinbase(v: &Value, now: i64, url: &str) -> Result<(f64, i64)> {
    let data = v.get("data");
    let text = |key: &str| data.and_then(|d| d.get(key)).and_then(Value::as_str).unwrap_or_default();
    let (base, currency) = (text("base"), text("currency"));
    if currency != "USD" || base.is_empty() || !url.contains(&format!("/{base}-{currency}/")) {
        return Err(anyhow!("coinbase answer is for {base}-{currency}, not for the pair asked"));
    }
    let usd = data.and_then(|d| d.get("amount")).and_then(number);
    Ok((positive(usd, "price")?, now))
}

/// The number found at `pointer` (RFC 6901, such as `/data/price`) in any JSON answer.
pub fn parse_pointer(v: &Value, now: i64, pointer: &str) -> Result<(f64, i64)> {
    let usd = v.pointer(pointer).and_then(number);
    Ok((positive(usd, "price").with_context(|| format!("at {pointer}"))?, now))
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
        let v = json!({"error": [], "result": {"XXBTZUSD": {"a": ["100251.0","1","1.0"], "b": ["100250.0","1","1.0"], "c": ["99000.0", "0.01"]}}});
        // The middle of bid and ask, not the last trade.
        assert_eq!(parse_kraken(&v, NOW).unwrap(), (100_250.5, NOW));
        assert!(parse_kraken(&json!({"error": ["EQuery:Unknown asset pair"], "result": {}}), NOW).is_err());
        // A market nobody quotes tightly gives no price.
        let wide = json!({"error": [], "result": {"XAUTUSD": {"a": ["4100.0","1","1.0"], "b": ["3900.0","1","1.0"]}}});
        assert!(parse_kraken(&wide, NOW).is_err());
        let crossed = json!({"error": [], "result": {"XAUTUSD": {"a": ["3900.0","1","1.0"], "b": ["4000.0","1","1.0"]}}});
        assert!(parse_kraken(&crossed, NOW).is_err());
    }

    #[test]
    fn bitstamp() {
        let v = json!({"last": "99000", "timestamp": "1793793590", "bid": "100099", "ask": "100101"});
        assert_eq!(parse_bitstamp(&v, NOW).unwrap(), (100_100.0, 1_793_793_590));
        // An unknown pair answers with the list of all pairs: not a price.
        assert!(parse_bitstamp(&json!([{"last": "1", "bid": "1", "ask": "1"}]), NOW).is_err());
    }

    #[test]
    fn coinbase() {
        let btc = "https://api.coinbase.com/v2/prices/BTC-USD/spot";
        let v = json!({"data": {"amount": "99990.12", "base": "BTC", "currency": "USD"}});
        assert_eq!(parse_coinbase(&v, NOW, btc).unwrap(), (99_990.12, NOW));
        let gold = json!({"data": {"amount": "3997.2", "base": "PAXG", "currency": "USD"}});
        assert_eq!(parse_coinbase(&gold, NOW, "https://api.coinbase.com/v2/prices/PAXG-USD/spot").unwrap(), (3_997.2, NOW));
        // An answer for another pair than the one asked.
        assert!(parse_coinbase(&gold, NOW, btc).is_err());
        assert!(parse_coinbase(&json!({"data": {"amount": "1", "base": "BTC", "currency": "EUR"}}), NOW, btc).is_err());
    }

    #[test]
    fn any_json_answer() {
        let v = json!({"data": {"price": "4005.1", "list": [1, 2.5]}});
        assert_eq!(parse_pointer(&v, NOW, "/data/price").unwrap(), (4_005.1, NOW));
        assert_eq!(parse_pointer(&v, NOW, "/data/list/1").unwrap(), (2.5, NOW));
        assert!(parse_pointer(&v, NOW, "/data/absent").is_err());
        assert!(parse_pointer(&json!({"p": -1}), NOW, "/p").is_err());
    }

    #[test]
    fn a_file_of_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sources.json");
        // The usual set, written out and read back: a starting point to edit.
        std::fs::write(&path, serde_json::to_string_pretty(&default_specs()).unwrap()).unwrap();
        let sources = sources_from_file(&path).unwrap();
        assert_eq!(sources.len(), default_specs().len());
        assert!(sources.iter().any(|s| s.name() == "xaut:kraken"));
        // Three families for gold, three markets for bitcoin.
        let specs = default_specs();
        let families: std::collections::BTreeSet<&str> =
            specs.iter().filter(|s| s.actif == AssetName::Or).map(|s| menger_core::price::family(&s.nom)).collect();
        assert_eq!(families.len(), 3);
        assert!(specs.iter().filter(|s| s.actif == AssetName::Btc).count() >= 3);
        // One's own source, in any JSON shape.
        std::fs::write(&path, r#"[{"nom":"maison","actif":"or","url":"https://example.org/or.json","format":"json","champ":"/prix"}]"#).unwrap();
        assert_eq!(sources_from_file(&path).unwrap()[0].name(), "maison");
        // Never in clear over the network, never without saying where the price is.
        for bad in [
            r#"[{"nom":"a","actif":"or","url":"http://example.org/or.json","format":"json","champ":"/prix"}]"#,
            r#"[{"nom":"a","actif":"or","url":"https://example.org/or.json","format":"json"}]"#,
            r#"[{"nom":"a","actif":"argent","url":"https://example.org/","format":"kraken"}]"#,
            "[]",
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(sources_from_file(&path).is_err(), "{bad}");
        }
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
