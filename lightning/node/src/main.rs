//! `menger-ln`: run a saver or a provider, or watch a demonstration on a private Bitcoin network.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::Network;
use menger_core::price::{Asset, PriceRules, Quote};
use menger_core::stability::{Action, Params, Position, Role};
use menger_core::unit::{format_mengers_fr, format_sats_fr as sats_fr, grams, parse_mengers, SatsPerMenger};
use menger_ln::api::{self, ApiContext, Sandbox};
use menger_ln::keys;
use menger_ln::party::{close_reason_text, short_id, MIN_ADJUST_SATS};
use menger_ln::prices::{self, Source};
use menger_ln::regtest::{free_port, Bitcoind};
use menger_ln::web::{self, ApiCall, ApiReply};
use menger_ln::{ChainSource, Party, PartyConfig, Report};
use serde::Deserialize;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
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
    /// Essai dans le navigateur, sur un réseau Bitcoin privé : un fournisseur, Alice et Bruno, avec des prix fictifs.
    Essai {
        /// Chemin de `bitcoind` (Bitcoin Core).
        #[arg(long)]
        bitcoind: PathBuf,
    },
    /// Fait tourner un épargnant ou un fournisseur. Tape « aide » pendant qu'il tourne.
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
    /// Épargnant : prix de la stabilité, en millionièmes par an (20000 = 2 % payés par l'épargnant).
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    rate_ppm: i64,
    /// Épargnant : son fournisseur, `clé@hôte:port`.
    #[arg(long)]
    fournisseur: Option<String>,
    /// Épargnant : ouvre un canal de ce montant (en sats) vers le fournisseur s'il n'y en a pas.
    #[arg(long, requires = "fournisseur")]
    canal: Option<u64>,
    /// Épargnant : garder les paiements reçus en bitcoin au lieu de les ajouter au solde stable.
    #[arg(long)]
    sans_stabilisation_auto: bool,
    /// Secondes entre deux vérifications.
    #[arg(long, default_value_t = 30)]
    every: u64,
    /// Adresse de l'application dans le navigateur, sur cet ordinateur ; `aucune` pour s'en passer.
    #[arg(long, default_value = "127.0.0.1:8421")]
    interface: String,
    /// Restaure un portefeuille depuis sa phrase de récupération, dans un dossier neuf. La phrase est
    /// demandée au démarrage, pour qu'elle ne reste pas dans l'historique du terminal.
    #[arg(long)]
    restaurer: bool,
}

const HELP: &str = "\
Commandes :
  etat                          ce que tu détiens
  adresse                       une adresse pour recevoir des bitcoins sur la chaîne
  canal CLÉ@HÔTE:PORT SATS      ouvre un canal avec tes propres bitcoins
  stabiliser SATS | tout        épargnant : garde ces sats stables en or
  facture MNGR                  une facture Lightning de ce montant en Mengers (ex. : facture 0,5)
  payer FACTURE                 épargnant : paie une facture Lightning depuis ton solde
  quitter";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn clock() -> String {
    chrono::Utc::now().format("%H:%M:%S").to_string()
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Demo { bitcoind } => demo(&bitcoind),
        Command::Essai { bitcoind } => essai(&bitcoind),
        Command::Run(args) => run(*args),
    }
}

fn parse_peer(spec: &str) -> Result<(PublicKey, String)> {
    let (key, addr) = spec.split_once('@').ok_or_else(|| anyhow!("attendu : clé@hôte:port"))?;
    let key = PublicKey::from_str(key).map_err(|e| anyhow!("clé invalide : {e}"))?;
    Ok((key, addr.to_string()))
}

fn action_text(a: &Action) -> String {
    match a {
        Action::Hold => "à l'équilibre".to_string(),
        Action::Pay { sats } => format!("paie {} sats", sats_fr(*sats)),
        Action::Await { owed_sats, .. } => format!("attend {} sats de l'autre partie", sats_fr(*owed_sats)),
        Action::Close { reason } => format!("ferme les canaux : {}", close_reason_text(reason)),
    }
}

fn describe(party: &Party, r: &Report) -> Vec<String> {
    let Some(price) = &r.price else {
        return vec![format!("en pause, pas de prix fiable : {}", r.price_error.clone().unwrap_or_default())];
    };
    let mut lines = vec![format!(
        "1 MNGR = {} sats{}",
        sats_fr(price.sats_per_menger.0),
        if price.gold_carried { " (or du vendredi)" } else { "" }
    )];
    for p in &r.peers {
        let who = if party.role() == Role::Provider { format!("épargnant {}… : ", short_id(&p.peer)) } else { String::new() };
        lines.push(format!(
            "  {who}{} MNGR ({} g d'or) = {} sats ; détenus {} ; {}",
            format_mengers_fr(p.micro_mengers),
            grams_fr(p.micro_mengers, 2),
            sats_fr(p.decision.target_sats),
            sats_fr(p.backing_sats),
            action_text(&p.decision.action)
        ));
    }
    lines
}

fn status(party: &Party, t: i64) -> String {
    let mut s = String::new();
    match party.role() {
        Role::Saver => {
            s += &format!("Solde dans les canaux : {} sats", sats_fr(party.balance()));
            match party.position() {
                Some(p) => {
                    let micro = p.micro_mengers(t);
                    s += &format!(", dont {} en or = {} MNGR ({} g d'or)", sats_fr(p.backing_sats), format_mengers_fr(micro), grams_fr(micro, 3));
                }
                None => s += ", rien de stable pour l'instant",
            }
            s += &format!("\nLibres : {} sats", sats_fr(party.free_sats()));
            if let Some(p) = party.pending() {
                s += &format!("\nDemande en attente de réponse : {:+} sats", p.delta_backing);
            }
            if let Some(spend) = &party.state.spend {
                s += &format!("\nPaiement en cours : {} sats", sats_fr(spend.sats));
            }
        }
        Role::Provider => {
            let savers: Vec<_> = party.state.peers.iter().filter_map(|(k, p)| Some((k, p.position.as_ref()?))).collect();
            s += &format!("Épargnants servis : {}", savers.len());
            for (key, p) in savers {
                let peer = PublicKey::from_str(key).ok();
                let ours = peer.map_or(0, |k| party.our_outbound(&k));
                s += &format!(
                    "\n  {}… : {} MNGR, {} sats chez lui, {} sats de notre côté pour couvrir les baisses",
                    short_id(key),
                    format_mengers_fr(p.micro_mengers(t)),
                    sats_fr(p.backing_sats),
                    sats_fr(ours)
                );
            }
        }
    }
    s += &format!("\nSur la chaîne : {} sats", sats_fr(party.onchain_spendable_sats()));
    s
}

/// Runs one interactive command; true to quit.
fn command(party: &mut Party, line: &str, sources: &[Box<dyn Source>]) -> Result<bool> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some((&cmd, rest)) = words.split_first() else { return Ok(false) };
    let t = now();
    let quotes = || {
        let (q, errors) = prices::read_all(sources, t);
        for e in errors {
            eprintln!("source de prix indisponible : {e}");
        }
        q
    };
    match (cmd, rest) {
        ("aide" | "?", _) => println!("{HELP}"),
        ("etat", _) => println!("{}", status(party, t)),
        ("adresse", _) => println!("Adresse pour recevoir des bitcoins : {}", party.new_address()?),
        ("canal", [spec, sats]) => {
            let (key, addr) = parse_peer(spec)?;
            party.open_channel(key, &addr, sats.parse().context("montant en sats")?)?;
            println!("Canal demandé : il sera prêt après confirmation sur la chaîne.");
        }
        ("stabiliser", [amount]) => {
            let sats = if *amount == "tout" { party.free_sats() } else { amount.parse().context("montant en sats")? };
            party.stabilize(sats, &quotes(), t)?;
            println!("Demande envoyée au fournisseur : {sats} sats à garder stables.");
        }
        ("facture", [amount]) => {
            let micro = parse_mengers(amount).ok_or_else(|| anyhow!("montant en Mengers attendu, par exemple 0,5"))?;
            let (invoice, sats) = party.invoice(micro, &quotes(), t)?;
            println!("Facture de {} MNGR ({} sats au prix actuel), valable une heure :\n{invoice}", format_mengers_fr(micro), sats_fr(sats));
        }
        ("payer", [invoice]) => {
            party.pay(invoice, &quotes(), t)?;
            println!("Paiement en préparation.");
        }
        ("quitter", _) => return Ok(true),
        _ => bail!("commande inconnue ou incomplète : {line} (tape « aide »)"),
    }
    Ok(false)
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
    let provider = a.fournisseur.as_deref().map(parse_peer).transpose()?;
    let mut cfg = PartyConfig::new(role, a.data.clone(), network, a.listen.clone(), chain);
    cfg.price_rules = PriceRules { min_sources: a.min_sources, ..PriceRules::default() };
    cfg.rate_ppm_per_year = a.rate_ppm;
    cfg.provider = provider.clone();
    cfg.auto_stabilize = !a.sans_stabilisation_auto;
    if a.restaurer {
        print!("Phrase de récupération (12 mots) : ");
        std::io::stdout().flush()?;
        let mut words = String::new();
        std::io::stdin().read_line(&mut words)?;
        cfg.restore_phrase = Some(words);
    }
    let mut party = Party::start(cfg)?;
    show_new_phrase(&party, &a.data);
    if a.restaurer {
        println!("Portefeuille restauré : il retrouve ses bitcoins sur la chaîne en se synchronisant.");
    }
    println!("Nœud {} ({})", party.node_id(), if role == Role::Saver { "épargnant" } else { "fournisseur" });
    println!("Adresse pour recevoir des bitcoins : {}", party.new_address()?);

    let (tx, rx) = mpsc::channel::<Input>();
    read_stdin(tx.clone());
    if a.interface != "aucune" {
        match open_interface(&a.interface, 0, &tx) {
            Ok(interface) => println!("Application : ouvre {} dans ton navigateur.", interface.url),
            Err(e) => eprintln!("Application indisponible : {e:#}"),
        }
    }
    drop(tx);
    println!("Tape « aide » pour la liste des commandes.");
    let fetch = || {
        let (q, errors) = prices::read_all(&sources, now());
        for e in errors {
            eprintln!("source de prix indisponible : {e}");
        }
        q
    };
    let mut last: Option<Report> = None;
    let mut inputs_open = true;
    let mut channel_wanted = a.canal;
    let mut next_tick = Instant::now();
    loop {
        if inputs_open {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(Input::Line(line)) => {
                    match command(&mut party, &line, &sources) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) => eprintln!("{e:#}"),
                    }
                    // Act at once on what the command started.
                    next_tick = Instant::now();
                    continue;
                }
                Ok(Input::Api(_, call)) => {
                    let ctx =
                        ApiContext { last: last.as_ref(), quotes: &fetch, now: now(), network, name: "", sandbox: None };
                    let reply = api::handle(&mut party, &call, &ctx);
                    if call.method == "POST" {
                        next_tick = Instant::now();
                    }
                    let _ = call.reply.send(reply);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => inputs_open = false,
            }
        } else {
            std::thread::sleep(next_tick.saturating_duration_since(Instant::now()));
        }
        next_tick = Instant::now() + Duration::from_secs(a.every);
        if let (Some(sats), Some((key, addr))) = (channel_wanted, &provider) {
            if party.node.list_channels().iter().any(|c| c.counterparty.node_id == *key) {
                channel_wanted = None;
            } else {
                match party.open_channel(*key, addr, sats) {
                    Ok(()) => {
                        println!("{} canal de {sats} sats demandé vers le fournisseur", clock());
                        channel_wanted = None;
                    }
                    Err(e) => eprintln!("{} canal pas encore ouvert : {e:#}", clock()),
                }
            }
        }
        let t = now();
        let quotes = fetch();
        match party.tick(&quotes, t) {
            Ok(r) => {
                for e in &r.events {
                    println!("{} {e}", clock());
                }
                for line in describe(&party, &r) {
                    println!("{} {line}", clock());
                }
                last = Some(r);
            }
            Err(e) => eprintln!("erreur : {e:#}"),
        }
    }
    party.stop()
}

/// What reaches the main loop: a line typed in the terminal, or a call from an app page (with
/// the number of the party it is for).
enum Input {
    Line(String),
    Api(usize, ApiCall),
}

fn read_stdin(tx: mpsc::Sender<Input>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(Input::Line(line)).is_err() {
                break;
            }
        }
    });
}

fn open_interface(addr: &str, who: usize, tx: &mpsc::Sender<Input>) -> Result<web::Interface> {
    let tx = tx.clone();
    web::serve(addr, move |call| tx.send(Input::Api(who, call)).is_ok())
}

fn show_new_phrase(party: &Party, data: &Path) {
    if let Some(phrase) = &party.new_phrase {
        println!("\nNouveau portefeuille. Sa phrase de récupération :\n\n    {phrase}\n");
        println!("Écris ces 12 mots sur papier et garde-les en lieu sûr : avec --restaurer, ils redonnent accès aux bitcoins du portefeuille.");
        println!(
            "Ils sont aussi dans {} : protège ce fichier. Pour l'argent des canaux, sauvegarde le dossier entier.\n",
            data.join(keys::PHRASE_FILE).display()
        );
    }
}

// ----- Demonstration -----

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

struct Who {
    p: Party,
    addr: String,
    name: &'static str,
}

fn start_party(dir: &Path, b: &Bitcoind, name: &'static str, role: Role, provider: Option<&Who>, params: &Params) -> Result<Who> {
    let addr = format!("127.0.0.1:{}", free_port());
    let chain = ChainSource::BitcoindRpc { host: "127.0.0.1".into(), port: b.rpc_port, user: b.user.clone(), password: b.password.clone() };
    let mut cfg = PartyConfig::new(role, dir.join(name.to_lowercase()), Network::Regtest, addr.clone(), chain);
    cfg.params = params.clone();
    cfg.provider = provider.map(|p| (p.p.node_id(), p.addr.clone()));
    Ok(Who { p: Party::start(cfg)?, addr, name })
}

/// Ticks everyone, showing what happens, until `done` holds.
fn until(all: &mut [&mut Who], q: &[Quote], t: i64, what: &str, mut done: impl FnMut(&[&mut Who]) -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        for w in all.iter_mut() {
            w.p.sync();
            for e in w.p.tick(q, t)?.events {
                println!("     {:<11} : {e}", w.name);
            }
        }
        if done(all) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("délai dépassé : {what}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn wait(what: &str, mut cond: impl FnMut() -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !cond() {
        if Instant::now() > deadline {
            bail!("délai dépassé : {what}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// The saver's position, when both ledgers agree and nothing is in progress.
fn agreed(saver: &Who, provider: &Who) -> Option<Position> {
    let (s, p) = (saver.p.node_id(), provider.p.node_id());
    let ours = saver.p.position()?.clone();
    let quiet = saver.p.pending().is_none()
        && saver.p.peer(&p).is_none_or(|x| x.in_flight.is_none())
        && provider.p.peer(&s).is_none_or(|x| x.in_flight.is_none());
    (provider.p.position_with(&s) == Some(&ours) && quiet).then_some(ours)
}

/// Within the 0.1 % tolerance (a payment closes the gap to half of it) and the 100-sat minimum payment.
fn at_par(saver: &Who, provider: &Who, price: SatsPerMenger, t: i64) -> bool {
    agreed(saver, provider).is_some_and(|pos| {
        let target = price.sats_for(pos.micro_mengers(t));
        pos.backing_sats.abs_diff(target) < (target / 1_000).max(100) + 100
    })
}

fn holding(w: &Who, t: i64) -> String {
    let micro = w.p.position().map_or(0, |p| p.micro_mengers(t));
    format!("{} MNGR ({} g d'or)", format_mengers_fr(micro), grams_fr(micro, 2))
}

/// Grams of gold with a French decimal comma.
fn grams_fr(micro: u64, decimals: usize) -> String {
    format!("{:.*}", decimals, grams(micro)).replace('.', ",")
}

/// A private network with a provider and two savers, Alice and Bruno. Each gets test bitcoins;
/// Alice and the provider open a channel to each other; the provider opens one to Bruno.
fn network(dir: &Path, b: &Bitcoind, params: &Params, say: bool) -> Result<(Who, Who, Who)> {
    let mut f = start_party(dir, b, "Fournisseur", Role::Provider, None, params)?;
    let mut alice = start_party(dir, b, "Alice", Role::Saver, Some(&f), params)?;
    let mut bruno = start_party(dir, b, "Bruno", Role::Saver, Some(&f), params)?;
    let (pk_f, pk_a, pk_b) = (f.p.node_id(), alice.p.node_id(), bruno.p.node_id());
    if say {
        println!("1. Un fournisseur et deux épargnants, Alice et Bruno, reçoivent des bitcoins de test.");
    }
    // In this order, all of Alice's and Bruno's coins are spendable from the start: no coin of
    // theirs matures later and blurs what they get back at the end.
    b.mine(101, &bruno.p.new_address()?)?;
    b.mine(101, &alice.p.new_address()?)?;
    b.mine(101, &f.p.new_address()?)?;
    wait("fonds", || {
        for w in [&f, &alice, &bruno] {
            w.p.sync();
        }
        f.p.onchain_spendable_sats() > 10_000_000 && alice.p.onchain_spendable_sats() > 10_000_000 && bruno.p.onchain_spendable_sats() > 100_000
    })?;
    if say {
        println!("2. Alice et le fournisseur ouvrent chacun un canal vers l'autre, avec leurs propres bitcoins (0,02 BTC chacun).");
        println!("   Bruno n'ouvre rien : le fournisseur ouvre un canal vers lui.");
    }
    alice.p.open_channel(pk_f, &f.addr, 2_000_000)?;
    f.p.open_channel(pk_a, &alice.addr, 2_000_000)?;
    // One opening at a time for the provider: its second waits for the first to confirm.
    wait("transactions d'ouverture", || b.mempool_size().unwrap_or(0) >= 2)?;
    b.mine(1, &alice.p.new_address()?)?;
    wait("première ouverture", || {
        f.p.sync();
        !f.p.opening_in_progress()
    })?;
    f.p.open_channel(pk_b, &bruno.addr, 2_000_000)?;
    wait("transaction d'ouverture vers Bruno", || b.mempool_size().unwrap_or(0) >= 1)?;
    b.mine(6, &alice.p.new_address()?)?;
    let t0 = now();
    let q0 = quotes(100_000.0, Some(4_319.0), t0);
    until(&mut [&mut f, &mut alice, &mut bruno], &q0, t0, "canaux", |w| {
        w[0].p.channels_with(&pk_a).len() == 2 && w[0].p.channels_with(&pk_b).len() == 1 && w[2].p.channels_with(&pk_f).len() == 1
    })?;
    Ok((f, alice, bruno))
}

/// The trial's market: the page moves the price of bitcoin or of gold by a percentage.
fn move_market(market: &mut (f64, f64), body: &str) -> ApiReply {
    #[derive(Deserialize)]
    struct Move {
        btc_pct: Option<f64>,
        or_pct: Option<f64>,
    }
    let Ok(m) = serde_json::from_str::<Move>(body) else { return ApiReply::error(400, "requête invalide") };
    let factor = |pct: Option<f64>| 1.0 + pct.unwrap_or(0.0).clamp(-50.0, 100.0) / 100.0;
    market.0 = (market.0 * factor(m.btc_pct)).max(1_000.0);
    market.1 = (market.1 * factor(m.or_pct)).max(100.0);
    ApiReply::ok(serde_json::json!({ "btc_usd": market.0, "or_usd": market.1 }))
}

fn essai(exe: &Path) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("menger-essai-{}", std::process::id()));
    let b = Bitcoind::start(exe, &dir.join("bitcoind"))?;
    println!("Essai de Menger sur un réseau Bitcoin privé, sur cet ordinateur : un fournisseur et deux épargnants, Alice et Bruno.");
    println!("Préparation, environ une minute…");
    // Payments at most every five seconds, so that each move of the price is easy to follow.
    let params = Params { cooldown_secs: 5, ..Params::default() };
    let (f, mut alice, bruno) = network(&dir, &b, &params, false)?;
    let t0 = now();
    alice.p.stabilize(1_500_000, &quotes(100_000.0, Some(4_319.0), t0), t0)?;
    let mut people = [f, alice, bruno];
    let names = ["le fournisseur", "Alice", "Bruno"];

    let (tx, rx) = mpsc::channel::<Input>();
    read_stdin(tx.clone());
    let mut urls = Vec::new();
    for (who, port) in [8420, 8421, 8422].into_iter().enumerate() {
        let interface =
            open_interface(&format!("127.0.0.1:{port}"), who, &tx).or_else(|_| open_interface("127.0.0.1:0", who, &tx))?;
        urls.push(interface.url);
    }
    drop(tx);
    println!("\nPrêt. Ouvre dans ton navigateur :");
    println!("  Alice        {}", urls[1]);
    println!("  Bruno        {}", urls[2]);
    println!("  Fournisseur  {}", urls[0]);
    println!("\nAlice garde 0,015 BTC en or. Fais bouger le prix du bitcoin avec les boutons en haut de la page,");
    println!("ou fais payer des Mengers à Bruno. Tape « quitter » pour arrêter.\n");

    let mut market = (100_000.0_f64, 4_319.0_f64);
    let mining_address = people[0].p.new_address()?;
    let mut last: [Option<Report>; 3] = [None, None, None];
    let mut next_tick = Instant::now();
    let mut next_block = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(Input::Line(line)) => {
                if line.trim() == "quitter" {
                    break;
                }
                println!("Tape « quitter » pour arrêter ; le reste se fait dans le navigateur.");
                continue;
            }
            Ok(Input::Api(who, call)) => {
                let reply = if call.method == "POST" && call.path == "/api/marche" {
                    move_market(&mut market, &call.body)
                } else {
                    let others = (0..3).filter(|&j| j != who).map(|j| (names[j].to_string(), urls[j].clone())).collect();
                    let sandbox = Sandbox { btc_usd: market.0, xau_usd: market.1, others };
                    let fetch = || quotes(market.0, Some(market.1), now());
                    let name = if who == 0 { "" } else { names[who] };
                    let ctx = ApiContext {
                        last: last[who].as_ref(),
                        quotes: &fetch,
                        now: now(),
                        network: Network::Regtest,
                        name,
                        sandbox: Some(&sandbox),
                    };
                    api::handle(&mut people[who].p, &call, &ctx)
                };
                if call.method == "POST" {
                    next_tick = Instant::now();
                }
                let _ = call.reply.send(reply);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        next_tick = Instant::now() + Duration::from_secs(2);
        // A block every ten seconds, so that what goes through the chain confirms on its own.
        if Instant::now() >= next_block {
            let _ = b.mine(1, &mining_address);
            next_block = Instant::now() + Duration::from_secs(10);
        }
        let t = now();
        let q = quotes(market.0, Some(market.1), t);
        for (i, w) in people.iter_mut().enumerate() {
            w.p.sync();
            match w.p.tick(&q, t) {
                Ok(r) => {
                    for e in &r.events {
                        println!("{} {:<11} : {e}", clock(), w.name);
                    }
                    last[i] = Some(r);
                }
                Err(e) => eprintln!("{} {:<11} : erreur : {e:#}", clock(), w.name),
            }
        }
    }
    for w in people {
        w.p.stop()?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn demo(exe: &Path) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("menger-demo-{}", std::process::id()));
    let b = Bitcoind::start(exe, &dir.join("bitcoind"))?;
    println!("Menger sur Lightning : démonstration sur un réseau Bitcoin privé\n");
    // Fast rules for the demonstration: no pause between payments, ten seconds of grace.
    let params = Params { cooldown_secs: 0, grace_secs: 10, ..Params::default() };
    let (mut f, mut alice, mut bruno) = network(&dir, &b, &params, true)?;
    let t0 = now();
    let q0 = quotes(100_000.0, Some(4_319.0), t0);

    println!("3. Bitcoin à 100 000 $, or à 4 319 $ l'once. Alice garde presque tout son solde stable en or.");
    let all = alice.p.balance();
    alice.p.stabilize(all - 1_000, &q0, t0)?;
    until(&mut [&mut f, &mut alice, &mut bruno], &q0, t0, "position d'Alice", |w| agreed(w[1], w[0]).is_some())?;
    println!("   → Alice : {}", holding(&alice, t0));

    println!("4. Bruno demande 0,5 MNGR. Alice paie sa facture ; le paiement passe par le fournisseur.");
    let (invoice, sats) = bruno.p.invoice(500_000, &q0, t0)?;
    println!("   Facture de Bruno : 0,5 MNGR, soit {} sats à ce prix.", sats_fr(sats));
    alice.p.pay(&invoice, &q0, t0)?;
    until(&mut [&mut f, &mut alice, &mut bruno], &q0, t0, "paiement", |w| {
        w[1].p.state.spend.is_none()
            && w[1].p.state.to_stabilize_sats < MIN_ADJUST_SATS
            && w[2].p.state.to_stabilize_sats == 0
            && agreed(w[1], w[0]).is_some()
            && agreed(w[2], w[0]).is_some()
    })?;
    println!("   → Alice : {} ; Bruno : {}", holding(&alice, t0), holding(&bruno, t0));

    let steps: [(&str, f64, f64, i64); 4] = [
        ("5. Le bitcoin baisse de 10 % (90 000 $) : le fournisseur complète chez les deux.", 90_000.0, 4_319.0, 600),
        ("6. Le bitcoin remonte à 108 000 $ : Alice et Bruno rendent l'excédent.", 108_000.0, 4_319.0, 1_200),
        ("7. L'or monte de 5 % : le fournisseur complète.", 108_000.0, 4_534.95, 1_800),
        ("8. Krach : le bitcoin tombe à 70 000 $ : le fournisseur complète.", 70_000.0, 4_534.95, 2_400),
    ];
    for (text, btc, gold, dt) in steps {
        println!("{text}");
        let t = t0 + dt;
        let price = SatsPerMenger::from_usd(btc, gold).ok_or_else(|| anyhow!("prix"))?;
        until(&mut [&mut f, &mut alice, &mut bruno], &quotes(btc, Some(gold), t), t, "équilibre", |w| {
            at_par(w[1], w[0], price, t) && at_par(w[2], w[0], price, t)
        })?;
        for w in [&alice, &bruno] {
            let p = w.p.position().ok_or_else(|| anyhow!("position"))?;
            println!("   → {} détient {} sats, soit toujours {}", w.name, sats_fr(p.backing_sats), holding(w, t));
        }
    }

    println!("9. Le fournisseur disparaît pendant une nouvelle baisse : Alice et Bruno ferment leurs canaux.");
    let t = t0 + 3_000;
    let q = quotes(60_000.0, Some(4_534.95), t);
    let before = [alice.p.onchain_spendable_sats(), bruno.p.onchain_spendable_sats()];
    for w in [&mut alice, &mut bruno] {
        w.p.tick(&q, t)?;
        for e in w.p.tick(&q, t + 11)?.events {
            println!("     {:<11} : {e}", w.name);
        }
    }
    wait("transactions de fermeture", || {
        for w in [&mut alice, &mut bruno] {
            let _ = w.p.tick(&q, t + 12);
        }
        b.mempool_size().unwrap_or(0) >= 3
    })?;
    b.mine(6, &f.p.new_address()?)?;
    wait("retour des fonds", || {
        for w in [&mut alice, &mut bruno] {
            w.p.sync();
            let _ = w.p.tick(&q, t + 12);
        }
        alice.p.onchain_spendable_sats() > before[0] && bruno.p.onchain_spendable_sats() > before[1]
    })?;
    println!(
        "   Revenu sur la chaîne : +{} sats pour Alice, +{} sats pour Bruno, qui n'avait jamais mis un bitcoin dans un canal\n   (chacun avec la réserve que son portefeuille gardait pour les frais de fermeture).",
        sats_fr(alice.p.onchain_spendable_sats() - before[0]),
        sats_fr(bruno.p.onchain_spendable_sats() - before[1])
    );
    println!("\nFin : à aucun moment un tiers n'a détenu les fonds, et chacun a utilisé ses propres prix.");
    for w in [f, alice, bruno] {
        w.p.stop()?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
