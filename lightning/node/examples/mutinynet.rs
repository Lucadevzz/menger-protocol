//! Épargnant et fournisseur sur Mutinynet (réseau Bitcoin de test public, un bloc toutes les
//! 30 secondes), avec les vrais prix du bitcoin et de l'or.
//!
//!   cargo run --example mutinynet -- adresses DOSSIER
//!   cargo run --example mutinynet -- lancer DOSSIER MINUTES
//!
//! Les deux nœuds tournent dans ce programme et se parlent par Lightning en local ; la chaîne
//! (financement, ouverture et fermeture des canaux) est la chaîne publique de Mutinynet.

use anyhow::{bail, Result};
use ldk_node::bitcoin::Network;
use menger_core::price::PriceRules;
use menger_core::stability::{Action, Params, Role};
use menger_core::unit::{format_mengers, grams};
use menger_ln::prices;
use menger_ln::{ChainSource, Party, PartyConfig};
use std::path::Path;
use std::time::{Duration, Instant};

const ESPLORA: &str = "https://mutinynet.com/api";
const PROVIDER_LISTEN: &str = "127.0.0.1:19735";
const SAVER_LISTEN: &str = "127.0.0.1:19736";
const CHANNEL_SATS: u64 = 400_000;
const STABLE_SATS: u64 = 200_000;

fn start(role: Role, dir: &Path) -> Result<Party> {
    let (name, listen) = match role {
        Role::Saver => ("epargnant", SAVER_LISTEN),
        Role::Provider => ("fournisseur", PROVIDER_LISTEN),
    };
    let chain = ChainSource::Esplora { url: ESPLORA.into() };
    let mut cfg = PartyConfig::new(role, dir.join(name), Network::Signet, listen.into(), chain);
    // Seuils fins pour voir les paiements suivre les petits mouvements réels du prix.
    cfg.params = Params { threshold_ppm: 100, min_payment_sats: 10, cooldown_secs: 30, ..Params::default() };
    cfg.price_rules = PriceRules::default();
    Party::start(cfg)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (cmd, dir) = match (args.get(1), args.get(2)) {
        (Some(c), Some(d)) => (c.as_str(), Path::new(d).to_path_buf()),
        _ => bail!("usage : mutinynet adresses|lancer DOSSIER [MINUTES]"),
    };
    let mut provider = start(Role::Provider, &dir)?;
    let mut saver = start(Role::Saver, &dir)?;
    provider.sync();
    saver.sync();
    if cmd == "adresses" {
        println!("fournisseur {} : {} sats, adresse {}", provider.node_id(), provider.onchain_spendable_sats(), provider.new_address()?);
        println!("épargnant   {} : {} sats, adresse {}", saver.node_id(), saver.onchain_spendable_sats(), saver.new_address()?);
        provider.stop()?;
        saver.stop()?;
        return Ok(());
    }
    let minutes: u64 = args.get(3).and_then(|m| m.parse().ok()).unwrap_or(20);
    let sources = prices::live_sources();
    let need = CHANNEL_SATS + 30_000;
    let deadline = Instant::now() + Duration::from_secs(600);
    while provider.onchain_spendable_sats() < need || saver.onchain_spendable_sats() < need {
        if Instant::now() > deadline {
            bail!("fonds insuffisants : fournisseur {} sats, épargnant {} sats", provider.onchain_spendable_sats(), saver.onchain_spendable_sats());
        }
        std::thread::sleep(Duration::from_secs(10));
        provider.sync();
        saver.sync();
    }
    println!("Fonds sur la chaîne : fournisseur {} sats, épargnant {} sats", provider.onchain_spendable_sats(), saver.onchain_spendable_sats());

    if provider.channels().is_empty() && saver.channels().is_empty() && provider.node.list_channels().is_empty() {
        saver.open_channel(provider.node_id(), PROVIDER_LISTEN, CHANNEL_SATS)?;
        provider.open_channel(saver.node_id(), SAVER_LISTEN, CHANNEL_SATS)?;
        println!("Canaux demandés : {CHANNEL_SATS} sats de chaque côté. Attente des confirmations (un bloc toutes les 30 s)…");
    }
    let t = now();
    let (q, _) = prices::read_all(&sources, t);
    let deadline = Instant::now() + Duration::from_secs(1_200);
    while saver.channels().len() < 2 || provider.channels().len() < 2 {
        if Instant::now() > deadline {
            bail!("canaux pas prêts après 20 minutes");
        }
        std::thread::sleep(Duration::from_secs(15));
        provider.sync();
        saver.sync();
        for e in saver.tick(&q, now())?.events.into_iter().chain(provider.tick(&q, now())?.events) {
            println!("  {e}");
        }
    }
    for c in saver.node.list_channels() {
        if let Some(txo) = c.funding_txo {
            println!("Canal ouvert : https://mutinynet.com/tx/{}", txo.txid);
        }
    }

    if saver.state.position.is_none() {
        let t = now();
        let (q, errors) = prices::read_all(&sources, t);
        for e in errors {
            println!("  source indisponible : {e}");
        }
        let p = saver.propose(STABLE_SATS, &q, t)?;
        println!("Position proposée : {} MNGR ({:.3} g d'or) pour {STABLE_SATS} sats", format_mengers(p.micro_at_start), grams(p.micro_at_start));
        let deadline = Instant::now() + Duration::from_secs(120);
        while saver.state.position.is_none() || provider.state.position.is_none() {
            if Instant::now() > deadline {
                bail!("pas d'accord sur la position");
            }
            std::thread::sleep(Duration::from_secs(2));
            for e in saver.tick(&q, t)?.events.into_iter().chain(provider.tick(&q, t)?.events) {
                println!("  {e}");
            }
        }
    }

    println!("\nSuivi en temps réel pendant {minutes} minutes, avec les vrais prix :");
    let end = Instant::now() + Duration::from_secs(minutes * 60);
    let mut payments = 0;
    while Instant::now() < end {
        let t = now();
        let (q, errors) = prices::read_all(&sources, t);
        for e in &errors {
            println!("  source indisponible : {e}");
        }
        let rs = saver.tick(&q, t)?;
        let rp = provider.tick(&q, t)?;
        for e in rs.events.iter().chain(rp.events.iter()) {
            if e.contains("envoyé") {
                payments += 1;
            }
            println!("  {e}");
        }
        match (&rs.price, &rs.decision) {
            (Some(p), Some(d)) => println!(
                "{} BTC {:.0} $, or {:.2} $/oz ({} sats/MNGR) : promesse {} sats, détenu {} sats, {}",
                chrono::Utc::now().format("%H:%M:%S"),
                p.btc_usd,
                p.xau_usd,
                p.sats_per_menger.0,
                d.target_sats,
                rs.backing_sats.unwrap_or(0),
                match &d.action {
                    Action::Hold => "équilibre".to_string(),
                    Action::Pay { sats } => format!("l'épargnant paie {sats} sats"),
                    Action::Await { owed_sats, .. } => format!("le fournisseur doit {owed_sats} sats"),
                    Action::Close { reason } => format!("fermeture : {reason:?}"),
                }
            ),
            _ => println!("{} en pause : {}", chrono::Utc::now().format("%H:%M:%S"), rs.price_error.unwrap_or_default()),
        }
        std::thread::sleep(Duration::from_secs(20));
    }
    let pos = saver.state.position.clone().unwrap();
    println!(
        "\nBilan : {payments} paiements de stabilité ; l'épargnant détient {} sats pour {} MNGR ; registres identiques : {}",
        pos.backing_sats,
        format_mengers(pos.micro_mengers(now())),
        saver.state.position == provider.state.position
    );
    provider.stop()?;
    saver.stop()?;
    Ok(())
}
