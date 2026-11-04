//! End to end on a private Bitcoin network: two real Lightning nodes, real channels, real
//! payments. Needs a `bitcoind` binary: `BITCOIND_EXE=/path/to/bitcoind cargo test -- --nocapture`.
//! Without it, the test says so and passes.

use ldk_node::bitcoin::Network;
use menger_core::price::{Asset, Quote};
use menger_core::stability::{Action, CloseReason, Params, Role};
use menger_core::unit::SatsPerMenger;
use menger_ln::regtest::{free_port, Bitcoind};
use menger_ln::{ChainSource, Party, PartyConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WED_NOON: i64 = 1_793_793_600; // Wednesday 4 November 2026, 12:00 UTC
const DAY: i64 = 86_400;
const CHANNEL_SATS: u64 = 2_000_000;
const STABLE_SATS: u64 = 1_000_000;

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

fn party(role: Role, dir: &Path, b: &Bitcoind) -> (Party, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let chain = ChainSource::BitcoindRpc {
        host: "127.0.0.1".into(),
        port: b.rpc_port,
        user: b.user.clone(),
        password: b.password.clone(),
    };
    let mut cfg = PartyConfig::new(role, dir.to_path_buf(), Network::Regtest, listen.clone(), chain);
    // Fast rules for the test: no pause between payments, ten seconds of grace.
    cfg.params = Params { cooldown_secs: 0, grace_secs: 10, ..Params::default() };
    (Party::start(cfg).expect("node starts"), listen)
}

/// Ticks both parties until `done` holds, or panics after `secs` seconds.
fn tick_until(
    saver: &mut Party,
    provider: &mut Party,
    q: &[Quote],
    now: i64,
    secs: u64,
    what: &str,
    mut done: impl FnMut(&Party, &Party) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let rs = saver.tick(q, now).expect("saver tick");
        let rp = provider.tick(q, now).expect("provider tick");
        for e in rs.events.iter() {
            println!("   [épargnant]   {e}");
        }
        for e in rp.events.iter() {
            println!("   [fournisseur] {e}");
        }
        if done(saver, provider) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for(secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn backing(p: &Party) -> u64 {
    p.state.position.as_ref().map(|x| x.backing_sats).unwrap_or(0)
}

#[test]
fn gold_balance_over_lightning() {
    let Some(exe) = std::env::var_os("BITCOIND_EXE").map(PathBuf::from) else {
        eprintln!("BITCOIND_EXE not set: end-to-end test skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let bitcoind = Bitcoind::start(&exe, &dir.path().join("bitcoind")).expect("bitcoind");
    let (mut saver, saver_addr) = party(Role::Saver, &dir.path().join("epargnant"), &bitcoind);
    let (mut provider, provider_addr) = party(Role::Provider, &dir.path().join("fournisseur"), &bitcoind);

    println!("1. Chacun reçoit des bitcoins sur le réseau privé.");
    bitcoind.mine(101, &saver.new_address().unwrap()).unwrap();
    bitcoind.mine(101, &provider.new_address().unwrap()).unwrap();
    wait_for(120, "on-chain funds", || {
        saver.sync();
        provider.sync();
        saver.onchain_spendable_sats() > 5 * CHANNEL_SATS && provider.onchain_spendable_sats() > 5 * CHANNEL_SATS
    });

    println!("2. Chacun ouvre un canal vers l'autre avec ses propres bitcoins.");
    let pk_s = saver.node_id();
    let pk_p = provider.node_id();
    saver.open_channel(pk_p, &provider_addr, CHANNEL_SATS).unwrap();
    provider.open_channel(pk_s, &saver_addr, CHANNEL_SATS).unwrap();
    wait_for(60, "funding transactions", || bitcoind.mempool_size().unwrap_or(0) >= 2);
    bitcoind.mine(6, &saver.new_address().unwrap()).unwrap();
    let q0 = quotes(100_000.0, Some(4_319.0), WED_NOON);
    tick_until(&mut saver, &mut provider, &q0, WED_NOON, 120, "two usable channels", |s, p| {
        s.sync();
        p.sync();
        s.channels().len() == 2 && p.channels().len() == 2
    });

    println!("3. L'épargnant propose de stabiliser {STABLE_SATS} sats en or ; le fournisseur accepte.");
    saver.propose(STABLE_SATS, &q0, WED_NOON).unwrap();
    tick_until(&mut saver, &mut provider, &q0, WED_NOON, 60, "position agreed", |s, p| {
        s.state.position.is_some() && p.state.position.is_some()
    });
    let micro = saver.state.position.as_ref().unwrap().micro_at_start;
    assert_eq!(saver.state.position, provider.state.position);
    let saver_sats_start = saver.our_balance_sats();

    let check_par = |s: &Party, p: &Party, price: SatsPerMenger| {
        let target = price.sats_for(micro);
        let b = backing(s);
        assert_eq!(b, backing(p), "both ledgers agree");
        assert!(b.abs_diff(target) <= target / 1000 + 100, "backing {b} vs target {target}");
    };

    println!("4. Le bitcoin baisse de 10 % : le fournisseur paie l'épargnant.");
    let t1 = WED_NOON + 600;
    let q1 = quotes(90_000.0, Some(4_319.0), t1);
    let p1 = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
    tick_until(&mut saver, &mut provider, &q1, t1, 60, "provider pays", |s, p| {
        s.state.in_flight.is_none()
            && p.state.in_flight.is_none()
            && backing(s) == backing(p)
            && backing(s).abs_diff(p1.sats_for(micro)) <= 100
    });
    check_par(&saver, &provider, p1);
    let gained = saver.our_balance_sats().saturating_sub(saver_sats_start);
    println!("   l'épargnant détient {gained} sats de plus dans le canal");
    assert!(gained.abs_diff(p1.sats_for(micro) - STABLE_SATS) <= 1_000, "gained {gained}");

    println!("5. Le bitcoin remonte de 20 % : l'épargnant rend l'excédent.");
    let t2 = t1 + 600;
    let q2 = quotes(108_000.0, Some(4_319.0), t2);
    let p2 = SatsPerMenger::from_usd(108_000.0, 4_319.0).unwrap();
    tick_until(&mut saver, &mut provider, &q2, t2, 60, "saver pays", |s, p| {
        s.state.in_flight.is_none() && backing(s) == backing(p) && backing(s).abs_diff(p2.sats_for(micro)) <= 100
    });
    check_par(&saver, &provider, p2);

    println!("6. L'or monte de 5 % : le fournisseur paie.");
    let t3 = t2 + 600;
    let q3 = quotes(108_000.0, Some(4_534.95), t3);
    let p3 = SatsPerMenger::from_usd(108_000.0, 4_534.95).unwrap();
    tick_until(&mut saver, &mut provider, &q3, t3, 60, "gold up", |s, p| {
        backing(s) == backing(p) && backing(s).abs_diff(p3.sats_for(micro)) <= 100
    });
    check_par(&saver, &provider, p3);

    println!("7. Week-end : l'or ne cote pas, le bitcoin baisse ; le dernier prix de l'or est gardé.");
    let sat = WED_NOON + 3 * DAY;
    let q4 = quotes(100_000.0, None, sat);
    let p4 = SatsPerMenger::from_usd(100_000.0, 4_534.95).unwrap();
    tick_until(&mut saver, &mut provider, &q4, sat, 60, "weekend", |s, p| {
        backing(s) == backing(p) && backing(s).abs_diff(p4.sats_for(micro)) <= 100
    });
    check_par(&saver, &provider, p4);

    println!("8. Le fournisseur cesse de payer pendant une baisse : l'épargnant ferme le canal.");
    let t5 = sat + 3_600;
    let q5 = quotes(85_000.0, None, t5);
    // Only the saver ticks: the provider is silent.
    let r = saver.tick(&q5, t5).unwrap();
    assert!(matches!(r.decision.as_ref().unwrap().action, Action::Await { .. }));
    let r = saver.tick(&q5, t5 + 11).unwrap();
    assert!(matches!(
        r.decision.as_ref().unwrap().action,
        Action::Close { reason: CloseReason::Unpaid { .. } }
    ));
    assert!(saver.state.closed.is_some());
    let onchain_before = saver.onchain_spendable_sats();
    tick_until(&mut saver, &mut provider, &q5, t5 + 12, 60, "closing transactions", |_, _| {
        bitcoind.mempool_size().unwrap_or(0) >= 1
    });
    bitcoind.mine(6, &provider.new_address().unwrap()).unwrap();
    wait_for(120, "saver's bitcoin back on chain", || {
        saver.sync();
        let _ = saver.tick(&q5, t5 + 12);
        saver.onchain_spendable_sats() > onchain_before + p4.sats_for(micro) * 9 / 10
    });
    println!(
        "   l'épargnant a récupéré ses bitcoins sur la chaîne : {} sats disponibles",
        saver.onchain_spendable_sats()
    );

    saver.stop().unwrap();
    provider.stop().unwrap();
}
