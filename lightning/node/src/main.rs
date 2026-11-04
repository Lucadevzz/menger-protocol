//! `menger-ln`: run a saver or a provider, or watch a demonstration on a private Bitcoin network.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::Network;
use menger_core::price::{Asset, PriceRules, Quote};
use menger_core::stability::{Action, Params, Role};
use menger_core::unit::{format_mengers, grams, SatsPerMenger};
use menger_ln::prices::{self, Source};
use menger_ln::regtest::{free_port, Bitcoind};
use menger_ln::{ChainSource, Party, PartyConfig, Report};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "menger-ln", about = "Un solde en or sur Lightning : 1 Menger = 3,545 g d'or, réglé en bitcoin")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    Epargnant,
    Fournisseur,
}

#[derive(Clone, Copy, ValueEnum)]
enum NetworkArg {
    Regtest,
    Signet,
    Testnet,
    Bitcoin,
}

#[derive(Subcommand)]
enum Command {
    /// Démonstration complète sur un réseau Bitcoin privé, sur cet ordinateur.
    Demo {
        /// Chemin de `bitcoind` (Bitcoin Core).
        #[arg(long)]
        bitcoind: PathBuf,
    },
    /// Fait tourner un épargnant ou un fournisseur.
    Run(Box<RunArgs>),
}

#[derive(clap::Args)]
struct RunArgs {
    #[arg(long, value_enum)]
    role: RoleArg,
    /// Dossier des clés et de l'état. À sauvegarder : il contient les clés du portefeuille.
    #[arg(long)]
    data: PathBuf,
    #[arg(long, value_enum, default_value = "signet")]
    network: NetworkArg,
    /// Adresse d'écoute Lightning.
    #[arg(long, default_value = "0.0.0.0:9735")]
    listen: String,
    /// Serveur Esplora (par exemple https://mutinynet.com/api).
    #[arg(long, conflicts_with = "bitcoind_rpc")]
    esplora: Option<String>,
    /// Nœud Bitcoin Core, `hôte:port`, avec --rpc-user et --rpc-password.
    #[arg(long)]
    bitcoind_rpc: Option<String>,
    #[arg(long, default_value = "")]
    rpc_user: String,
    #[arg(long, default_value = "")]
    rpc_password: String,
    /// `live` pour les prix publics, ou `fichier:CHEMIN` pour un fichier {"btc_usd":…,"xau_usd":…}.
    #[arg(long, default_value = "live")]
    prices: String,
    /// Sources qui doivent concorder pour chaque prix.
    #[arg(long, default_value_t = 2)]
    min_sources: usize,
    /// Prix de la stabilité, en millionièmes par an (20000 = 2 % payés par l'épargnant).
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    rate_ppm: i64,
    /// Ouvre un canal financé par nos bitcoins : `clé@hôte:port:sats`.
    #[arg(long)]
    open: Option<String>,
    /// Épargnant : sats à garder stables en or.
    #[arg(long)]
    stabilize: Option<u64>,
    /// Secondes entre deux vérifications.
    #[arg(long, default_value_t = 30)]
    every: u64,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Demo { bitcoind } => demo(&bitcoind),
        Command::Run(args) => run(*args),
    }
}

fn describe(r: &Report) -> String {
    let (Some(price), Some(d)) = (&r.price, &r.decision) else {
        return match &r.price_error {
            Some(e) => format!("en pause, pas de prix fiable : {e}"),
            None => "pas de position".into(),
        };
    };
    let micro = r.micro_mengers.unwrap_or(0);
    let action = match &d.action {
        Action::Hold => "à l'équilibre".to_string(),
        Action::Pay { sats } => format!("paie {sats} sats"),
        Action::Await { owed_sats, .. } => format!("attend {owed_sats} sats de l'autre partie"),
        Action::Close { reason } => format!("ferme le canal ({reason:?})"),
    };
    format!(
        "{} MNGR ({:.2} g d'or) = {} sats au prix de {} sats par Menger{} ; détenus {} ; {action}",
        format_mengers(micro),
        grams(micro),
        d.target_sats,
        price.sats_per_menger.0,
        if price.gold_carried { " (or du vendredi)" } else { "" },
        r.backing_sats.unwrap_or(0),
    )
}

fn run(a: RunArgs) -> Result<()> {
    let role = match a.role {
        RoleArg::Epargnant => Role::Saver,
        RoleArg::Fournisseur => Role::Provider,
    };
    let network = match a.network {
        NetworkArg::Regtest => Network::Regtest,
        NetworkArg::Signet => Network::Signet,
        NetworkArg::Testnet => Network::Testnet,
        NetworkArg::Bitcoin => Network::Bitcoin,
    };
    if network == Network::Bitcoin {
        eprintln!("ATTENTION : vrai réseau Bitcoin. Code non audité : n'y mets que ce que tu peux perdre.");
    }
    let chain = match (&a.esplora, &a.bitcoind_rpc) {
        (Some(url), _) => ChainSource::Esplora { url: url.clone() },
        (None, Some(hp)) => {
            let (host, port) = hp.rsplit_once(':').ok_or_else(|| anyhow!("--bitcoind-rpc attend hôte:port"))?;
            ChainSource::BitcoindRpc {
                host: host.into(),
                port: port.parse().context("port")?,
                user: a.rpc_user.clone(),
                password: a.rpc_password.clone(),
            }
        }
        (None, None) => bail!("indique --esplora ou --bitcoind-rpc"),
    };
    let sources: Vec<Box<dyn Source>> = match a.prices.as_str() {
        "live" => prices::live_sources(),
        other => match other.strip_prefix("fichier:") {
            Some(path) => vec![Box::new(prices::File { path: PathBuf::from(path) })],
            None => bail!("--prices attend `live` ou `fichier:CHEMIN`"),
        },
    };
    let mut cfg = PartyConfig::new(role, a.data.clone(), network, a.listen.clone(), chain);
    cfg.price_rules = PriceRules { min_sources: a.min_sources, ..PriceRules::default() };
    cfg.rate_ppm_per_year = a.rate_ppm;
    let mut party = Party::start(cfg)?;
    println!("Nœud {} ({})", party.node_id(), if role == Role::Saver { "épargnant" } else { "fournisseur" });
    println!("Adresse pour recevoir des bitcoins : {}", party.new_address()?);

    if let Some(spec) = &a.open {
        let (key, rest) = spec.split_once('@').ok_or_else(|| anyhow!("--open attend clé@hôte:port:sats"))?;
        let (addr, sats) = rest.rsplit_once(':').ok_or_else(|| anyhow!("--open attend clé@hôte:port:sats"))?;
        let peer = PublicKey::from_str(key).map_err(|e| anyhow!("clé invalide : {e}"))?;
        party.open_channel(peer, addr, sats.parse().context("montant")?)?;
        println!("Canal demandé vers {key} ({sats} sats).");
    }
    let mut proposed = a.stabilize.is_none();
    loop {
        let t = now();
        let (quotes, errors) = prices::read_all(&sources, t);
        for e in errors {
            eprintln!("source de prix indisponible : {e}");
        }
        if !proposed && !party.channels().is_empty() {
            match party.propose(a.stabilize.unwrap(), &quotes, t) {
                Ok(p) => println!("Position proposée : {} MNGR", format_mengers(p.micro_at_start)),
                Err(e) => eprintln!("proposition impossible pour l'instant : {e:#}"),
            }
            proposed = true;
        }
        match party.tick(&quotes, t) {
            Ok(r) => {
                for e in &r.events {
                    println!("{} {e}", chrono::Utc::now().format("%H:%M:%S"));
                }
                println!("{} {}", chrono::Utc::now().format("%H:%M:%S"), describe(&r));
            }
            Err(e) => eprintln!("erreur : {e:#}"),
        }
        std::thread::sleep(Duration::from_secs(a.every));
    }
}

fn quotes(btc: f64, gold: Option<f64>, at: i64) -> Vec<Quote> {
    let mut q = vec![
        Quote { source: "btc-a".into(), asset: Asset::Btc, usd: btc, at },
        Quote { source: "btc-b".into(), asset: Asset::Btc, usd: btc, at },
    ];
    if let Some(g) = gold {
        q.push(Quote { source: "or-a".into(), asset: Asset::Gold, usd: g, at });
        q.push(Quote { source: "or-b".into(), asset: Asset::Gold, usd: g, at });
    }
    q
}

fn settle(saver: &mut Party, provider: &mut Party, q: &[Quote], t: i64, price: SatsPerMenger) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let rs = saver.tick(q, t)?;
        let rp = provider.tick(q, t)?;
        for e in &rs.events {
            println!("     épargnant   : {e}");
        }
        for e in &rp.events {
            println!("     fournisseur : {e}");
        }
        let (Some(s), Some(p)) = (&saver.state.position, &provider.state.position) else { bail!("pas de position") };
        let target = price.sats_for(s.micro_mengers(t));
        if s.backing_sats == p.backing_sats && s.backing_sats.abs_diff(target) <= target / 1000 + 100 && saver.state.in_flight.is_none() && provider.state.in_flight.is_none() {
            println!("     → l'épargnant détient {} sats, soit toujours {} MNGR", s.backing_sats, format_mengers(s.micro_mengers(t)));
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("pas d'équilibre après 60 secondes");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn demo(exe: &std::path::Path) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("menger-demo-{}", std::process::id()));
    let b = Bitcoind::start(exe, &dir.join("bitcoind"))?;
    let start = |role: Role, name: &str| -> Result<(Party, String)> {
        let listen = format!("127.0.0.1:{}", free_port());
        let chain = ChainSource::BitcoindRpc { host: "127.0.0.1".into(), port: b.rpc_port, user: b.user.clone(), password: b.password.clone() };
        let mut cfg = PartyConfig::new(role, dir.join(name), Network::Regtest, listen.clone(), chain);
        cfg.params = Params { cooldown_secs: 0, grace_secs: 10, ..Params::default() };
        Ok((Party::start(cfg)?, listen))
    };
    let (mut saver, saver_addr) = start(Role::Saver, "epargnant")?;
    let (mut provider, provider_addr) = start(Role::Provider, "fournisseur")?;
    let wait = |what: &str, mut cond: Box<dyn FnMut() -> bool + '_>| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !cond() {
            if Instant::now() > deadline {
                bail!("délai dépassé : {what}");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    };

    println!("Menger sur Lightning : démonstration sur un réseau Bitcoin privé\n");
    println!("1. L'épargnant et le fournisseur reçoivent chacun des bitcoins de test.");
    b.mine(101, &saver.new_address()?)?;
    b.mine(101, &provider.new_address()?)?;
    wait("fonds", Box::new(|| {
        saver.sync();
        provider.sync();
        saver.onchain_spendable_sats() > 10_000_000 && provider.onchain_spendable_sats() > 10_000_000
    }))?;

    println!("2. Chacun ouvre un canal Lightning vers l'autre, avec ses propres bitcoins (0,02 BTC chacun).");
    let (pk_s, pk_p) = (saver.node_id(), provider.node_id());
    saver.open_channel(pk_p, &provider_addr, 2_000_000)?;
    provider.open_channel(pk_s, &saver_addr, 2_000_000)?;
    wait("transactions d'ouverture", Box::new(|| b.mempool_size().unwrap_or(0) >= 2))?;
    b.mine(6, &saver.new_address()?)?;
    let t0 = now();
    let q0 = quotes(100_000.0, Some(4_319.0), t0);
    let deadline = Instant::now() + Duration::from_secs(120);
    while saver.channels().len() < 2 || provider.channels().len() < 2 {
        saver.sync();
        provider.sync();
        saver.tick(&q0, t0)?;
        provider.tick(&q0, t0)?;
        if Instant::now() > deadline {
            bail!("canaux pas prêts");
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    println!("3. Bitcoin à 100 000 $, or à 4 319 $ l'once. L'épargnant veut garder 0,01 BTC stable en or.");
    let p0 = saver.propose(1_000_000, &q0, t0)?;
    println!("   Il propose {} MNGR, soit {:.2} g d'or.", format_mengers(p0.micro_at_start), grams(p0.micro_at_start));
    let deadline = Instant::now() + Duration::from_secs(60);
    while saver.state.position.is_none() || provider.state.position.is_none() {
        for e in saver.tick(&q0, t0)?.events.into_iter().chain(provider.tick(&q0, t0)?.events) {
            println!("     {e}");
        }
        if Instant::now() > deadline {
            bail!("pas d'accord sur la position");
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let steps: [(&str, f64, Option<f64>, i64); 4] = [
        ("4. Le bitcoin baisse de 10 % (90 000 $) : le fournisseur complète.", 90_000.0, Some(4_319.0), 600),
        ("5. Le bitcoin remonte à 108 000 $ : l'épargnant rend l'excédent.", 108_000.0, Some(4_319.0), 1_200),
        ("6. L'or monte de 5 % : le fournisseur complète.", 108_000.0, Some(4_534.95), 1_800),
        ("7. Krach : le bitcoin tombe à 70 000 $ : le fournisseur complète.", 70_000.0, Some(4_534.95), 2_400),
    ];
    for (text, btc, gold, dt) in steps {
        println!("{text}");
        let t = t0 + dt;
        let price = SatsPerMenger::from_usd(btc, gold.unwrap_or(4_319.0)).ok_or_else(|| anyhow!("prix"))?;
        settle(&mut saver, &mut provider, &quotes(btc, gold, t), t, price)?;
    }

    println!("8. Le fournisseur disparaît pendant une nouvelle baisse : l'épargnant ferme le canal et repart avec ses bitcoins.");
    let t = t0 + 3_000;
    let q = quotes(60_000.0, Some(4_534.95), t);
    println!("   Avant : l'épargnant a {} sats dans ses canaux.", saver.our_balance_sats());
    saver.tick(&q, t)?;
    let r = saver.tick(&q, t + 11)?;
    for e in r.events {
        println!("     épargnant : {e}");
    }
    let before = saver.onchain_spendable_sats();
    wait("transactions de fermeture", Box::new(|| {
        let _ = saver.tick(&q, t + 12);
        let _ = provider.tick(&q, t + 12);
        b.mempool_size().unwrap_or(0) >= 1
    }))?;
    b.mine(6, &provider.new_address()?)?;
    wait("retour des fonds", Box::new(|| {
        saver.sync();
        let _ = saver.tick(&q, t + 12);
        saver.onchain_spendable_sats() > before
    }))?;
    println!(
        "   Revenu sur la chaîne : +{} sats pour l'épargnant (avec la réserve que son portefeuille gardait pour les frais de fermeture).",
        saver.onchain_spendable_sats() - before
    );
    println!("\nFin : à aucun moment un tiers n'a détenu les fonds, et chacun a utilisé ses propres prix.");
    saver.stop()?;
    provider.stop()?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
