//! End to end on a private Bitcoin network: real Lightning nodes, real channels, real payments.
//! Needs a `bitcoind` binary: `BITCOIND_EXE=/path/to/bitcoind cargo test -- --nocapture`.
//! Without it, the tests say so and pass.

use ldk_node::bitcoin::Network;
use menger_core::price::{Asset, Quote};
use menger_core::stability::{Action, CloseReason, Params, Position, Role};
use menger_core::unit::{format_mengers, SatsPerMenger};
use menger_ln::party::{ReturnChannel, MIN_ADJUST_SATS, SPEND_TIMEOUT_SECS};
use menger_ln::regtest::{free_port, Bitcoind};
use menger_ln::{ChainSource, Party, PartyConfig, Report};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WED_NOON: i64 = 1_793_793_600; // Wednesday 4 November 2026, 12:00 UTC
const DAY: i64 = 86_400;
const CHANNEL_SATS: u64 = 2_000_000;

fn bitcoind_exe() -> Option<PathBuf> {
    let exe = std::env::var_os("BITCOIND_EXE").map(PathBuf::from);
    if exe.is_none() {
        eprintln!("BITCOIND_EXE not set: end-to-end test skipped");
    }
    exe
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

struct Peer {
    p: Party,
    addr: String,
    name: &'static str,
    cfg: PartyConfig,
}

fn start(name: &'static str, role: Role, dir: &Path, b: &Bitcoind, provider: Option<&Peer>) -> Peer {
    start_with(name, role, dir, b, provider, |_| {})
}

fn start_with(
    name: &'static str,
    role: Role,
    dir: &Path,
    b: &Bitcoind,
    provider: Option<&Peer>,
    tweak: impl FnOnce(&mut PartyConfig),
) -> Peer {
    let addr = format!("127.0.0.1:{}", free_port());
    let chain = ChainSource::BitcoindRpc {
        host: "127.0.0.1".into(),
        port: b.rpc_port,
        user: b.user.clone(),
        password: b.password.clone(),
    };
    let mut cfg = PartyConfig::new(role, dir.join(name), Network::Regtest, Some(addr.clone()), chain);
    // Fast rules for the test: no pause between payments, ten seconds of grace.
    cfg.params = Params { cooldown_secs: 0, grace_secs: 10, ..cfg.params };
    // The tests jump through time on purpose: no "back from a sleep" here, unless a test asks.
    cfg.max_tick_gap_secs = None;
    cfg.provider = provider.map(|p| (p.p.node_id(), p.addr.clone()));
    tweak(&mut cfg);
    Peer { p: Party::start(cfg.clone()).expect("node starts"), addr, name, cfg }
}

/// Stops a party's node and starts it again from what it saved, as after a power cut.
fn restart(peer: Peer) -> Peer {
    let Peer { p, addr, name, cfg } = peer;
    p.stop().expect("node stops");
    Peer { p: Party::start(cfg.clone()).expect("node starts again"), addr, name, cfg }
}

/// Ticks each party with its own view of the prices until `done` holds.
fn tick_views(views: &mut [(&mut Peer, &[Quote])], now: i64, secs: u64, what: &str, mut done: impl FnMut(&[&Peer]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        for (peer, q) in views.iter_mut() {
            peer.p.sync();
            for e in peer.p.tick(q, now).expect("tick").events {
                println!("   [{}] {e}", peer.name);
            }
        }
        let peers: Vec<&Peer> = views.iter().map(|(p, _)| &**p).collect();
        if done(&peers) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn said(peer: &Peer, text: &str) -> bool {
    peer.p.state.journal.iter().any(|e| e.text.contains(text))
}

/// Ticks every party until `done` holds, or panics after `secs` seconds.
fn tick_until(
    peers: &mut [&mut Peer],
    q: &[Quote],
    now: i64,
    secs: u64,
    what: &str,
    mut done: impl FnMut(&[&mut Peer]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        for peer in peers.iter_mut() {
            peer.p.sync();
            let r = peer.p.tick(q, now).expect("tick");
            for e in &r.events {
                println!("   [{}] {e}", peer.name);
            }
        }
        if done(peers) {
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

/// The saver's position, when its ledger and its provider's agree and nothing is in progress.
fn agreed(saver: &Peer, provider: &Peer) -> Option<Position> {
    let (s, p) = (saver.p.node_id(), provider.p.node_id());
    let ours = saver.p.position()?.clone();
    let theirs = provider.p.position_with(&s)?.clone();
    let quiet = saver.p.pending().is_none()
        && saver.p.peer(&p).is_none_or(|x| x.in_flight.is_none())
        && provider.p.peer(&s).is_none_or(|x| x.in_flight.is_none());
    (ours == theirs && quiet).then_some(ours)
}

/// Both ledgers agree and the saver holds the value of its Mengers at `price`, within the 0.1 %
/// tolerance (a payment closes the gap to half of it) and the 100-sat minimum payment.
fn at_par(saver: &Peer, provider: &Peer, price: SatsPerMenger, now: i64) -> bool {
    agreed(saver, provider).is_some_and(|pos| {
        let target = price.sats_for(pos.micro_mengers(now));
        pos.backing_sats.abs_diff(target) < (target / 1_000).max(100) + 100
    })
}

fn mengers(p: &Peer, now: i64) -> u64 {
    p.p.position().map_or(0, |x| x.micro_mengers(now))
}

fn action(r: &Report) -> &Action {
    &r.peers.first().expect("one position").decision.action
}

#[test]
fn gold_balance_over_lightning() {
    let Some(exe) = bitcoind_exe() else { return };
    let dir = tempfile::tempdir().unwrap();
    let bitcoind = Bitcoind::start(&exe, &dir.path().join("bitcoind")).expect("bitcoind");
    let mut provider = start("fournisseur", Role::Provider, dir.path(), &bitcoind, None);
    let mut saver = start("epargnant", Role::Saver, dir.path(), &bitcoind, Some(&provider));
    let (pk_s, pk_p) = (saver.p.node_id(), provider.p.node_id());

    println!("1. Chacun reçoit des bitcoins sur le réseau privé.");
    bitcoind.mine(101, &saver.p.new_address().unwrap()).unwrap();
    bitcoind.mine(101, &provider.p.new_address().unwrap()).unwrap();
    wait_for(120, "on-chain funds", || {
        saver.p.sync();
        provider.p.sync();
        saver.p.onchain_spendable_sats() > 5 * CHANNEL_SATS && provider.p.onchain_spendable_sats() > 5 * CHANNEL_SATS
    });

    println!("2. Chacun ouvre un canal vers l'autre avec ses propres bitcoins.");
    saver.p.open_channel(pk_p, &provider.addr, CHANNEL_SATS).unwrap();
    provider.p.open_channel(pk_s, &saver.addr, CHANNEL_SATS).unwrap();
    wait_for(60, "funding transactions", || bitcoind.mempool_size().unwrap_or(0) >= 2);
    bitcoind.mine(6, &saver.p.new_address().unwrap()).unwrap();
    let q0 = quotes(100_000.0, Some(4_319.0), WED_NOON);
    tick_until(&mut [&mut saver, &mut provider], &q0, WED_NOON, 120, "two usable channels", |n| {
        n[0].p.channels_with(&pk_p).len() == 2 && n[1].p.channels_with(&pk_s).len() == 2
    });

    let stable = 1_000_000;
    println!("3. L'épargnant demande à stabiliser {stable} sats en or ; le fournisseur accepte.");
    saver.p.stabilize(stable, &q0, WED_NOON).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q0, WED_NOON, 60, "position agreed", |n| agreed(n[0], n[1]).is_some());
    let micro = saver.p.position().unwrap().micro_at_start;
    println!("   position : {} MNGR", format_mengers(micro));
    let saver_sats_start = saver.p.balance();

    println!("4. Le bitcoin baisse de 10 % : le fournisseur paie l'épargnant.");
    let t1 = WED_NOON + 600;
    let q1 = quotes(90_000.0, Some(4_319.0), t1);
    let p1 = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q1, t1, 60, "provider pays", |n| at_par(n[0], n[1], p1, t1));
    let gained = saver.p.balance().saturating_sub(saver_sats_start);
    println!("   l'épargnant détient {gained} sats de plus dans ses canaux");
    assert!(gained.abs_diff(p1.sats_for(micro) - stable) <= 1_000, "gained {gained}");

    println!("5. Le bitcoin remonte de 20 % : l'épargnant rend l'excédent.");
    let t2 = t1 + 600;
    let q2 = quotes(108_000.0, Some(4_319.0), t2);
    let p2 = SatsPerMenger::from_usd(108_000.0, 4_319.0).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q2, t2, 60, "saver pays", |n| at_par(n[0], n[1], p2, t2));

    println!("6. L'or monte de 5 % : le fournisseur paie.");
    let t3 = t2 + 600;
    let q3 = quotes(108_000.0, Some(4_534.95), t3);
    let p3 = SatsPerMenger::from_usd(108_000.0, 4_534.95).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q3, t3, 60, "gold up", |n| at_par(n[0], n[1], p3, t3));

    println!("7. Week-end : l'or ne cote pas, le bitcoin baisse ; le dernier prix de l'or est gardé.");
    let sat = WED_NOON + 3 * DAY;
    let q4 = quotes(100_000.0, None, sat);
    let p4 = SatsPerMenger::from_usd(100_000.0, 4_534.95).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q4, sat, 60, "weekend", |n| at_par(n[0], n[1], p4, sat));

    println!("7b. L'épargnant clôt sa position alors qu'il doit quelques sats : il les paie d'abord, puis la clôture passe.");
    // Bitcoin up 0.08 %: less than the tolerance, so nobody paid, but the saver owes a little.
    let t6 = sat + 60;
    let q6 = quotes(100_080.0, None, t6);
    saver.p.release(None, &q6, t6).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q6, t6, 60, "position closed", |n| {
        n[0].p.position().is_none() && n[0].p.pending().is_none() && n[1].p.position_with(&pk_s).is_none()
    });
    assert!(said(&provider, "règle d'abord"), "refused first, for the debt");
    assert!(said(&saver, "payés au fournisseur pour clore la position"));
    saver.p.stabilize(stable, &q6, t6 + 1).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q6, t6 + 1, 60, "a new position", |n| agreed(n[0], n[1]).is_some());
    let micro = saver.p.position().unwrap().micro_at_start;

    println!("8. Le fournisseur cesse de payer pendant une baisse : l'épargnant ferme ses canaux.");
    let t5 = sat + 3_600;
    let q5 = quotes(85_000.0, None, t5);
    // Only the saver ticks: the provider is silent.
    let r = saver.p.tick(&q5, t5).unwrap();
    assert!(matches!(action(&r), Action::Await { .. }));
    let r = saver.p.tick(&q5, t5 + 11).unwrap();
    assert!(matches!(action(&r), Action::Close { reason: CloseReason::Unpaid { .. } }));
    assert!(saver.p.peer(&pk_p).unwrap().closed.is_some());
    let onchain_before = saver.p.onchain_spendable_sats();
    tick_until(&mut [&mut saver, &mut provider], &q5, t5 + 12, 60, "closing transactions", |_| {
        bitcoind.mempool_size().unwrap_or(0) >= 1
    });
    bitcoind.mine(6, &provider.p.new_address().unwrap()).unwrap();
    wait_for(120, "saver's bitcoin back on chain", || {
        saver.p.sync();
        let _ = saver.p.tick(&q5, t5 + 12);
        saver.p.onchain_spendable_sats() > onchain_before + p4.sats_for(micro) * 9 / 10
    });
    println!("   l'épargnant a récupéré ses bitcoins sur la chaîne : {} sats disponibles", saver.p.onchain_spendable_sats());

    saver.p.stop().unwrap();
    provider.p.stop().unwrap();
}

#[test]
fn paying_in_mengers_through_the_provider() {
    let Some(exe) = bitcoind_exe() else { return };
    let dir = tempfile::tempdir().unwrap();
    let bitcoind = Bitcoind::start(&exe, &dir.path().join("bitcoind")).expect("bitcoind");
    let mut provider = start("fournisseur", Role::Provider, dir.path(), &bitcoind, None);
    let mut alice = start("alice", Role::Saver, dir.path(), &bitcoind, Some(&provider));
    let mut bruno = start("bruno", Role::Saver, dir.path(), &bitcoind, Some(&provider));
    let (pk_p, pk_a, pk_b) = (provider.p.node_id(), alice.p.node_id(), bruno.p.node_id());
    let t = WED_NOON;
    let q0 = quotes(100_000.0, Some(4_319.0), t);

    println!("1. Des bitcoins sur la chaîne pour les trois.");
    bitcoind.mine(101, &provider.p.new_address().unwrap()).unwrap();
    bitcoind.mine(101, &alice.p.new_address().unwrap()).unwrap();
    bitcoind.mine(101, &bruno.p.new_address().unwrap()).unwrap();
    wait_for(120, "on-chain funds", || {
        for x in [&provider, &alice, &bruno] {
            x.p.sync();
        }
        provider.p.onchain_spendable_sats() > 5 * CHANNEL_SATS
            && alice.p.onchain_spendable_sats() > 5 * CHANNEL_SATS
            && bruno.p.onchain_spendable_sats() > 100_000
    });

    println!("2. Alice et le fournisseur ouvrent chacun un canal vers l'autre ; Bruno n'en ouvre aucun, le fournisseur en ouvre un vers lui.");
    alice.p.open_channel(pk_p, &provider.addr, CHANNEL_SATS).unwrap();
    provider.p.open_channel(pk_a, &alice.addr, CHANNEL_SATS).unwrap();
    // A second opening before the wallet has seen the first funding transaction could spend the
    // same coins: it is refused until then.
    assert!(provider.p.open_channel(pk_b, &bruno.addr, CHANNEL_SATS).is_err());
    wait_for(60, "first funding transactions", || bitcoind.mempool_size().unwrap_or(0) >= 2);
    bitcoind.mine(1, &alice.p.new_address().unwrap()).unwrap();
    wait_for(60, "first funding transaction known", || {
        provider.p.sync();
        !provider.p.opening_in_progress()
    });
    provider.p.open_channel(pk_b, &bruno.addr, CHANNEL_SATS).unwrap();
    wait_for(60, "funding transaction for Bruno", || bitcoind.mempool_size().unwrap_or(0) >= 1);
    bitcoind.mine(6, &alice.p.new_address().unwrap()).unwrap();
    tick_until(&mut [&mut provider, &mut alice, &mut bruno], &q0, t, 120, "channels", |n| {
        n[0].p.channels_with(&pk_a).len() == 2 && n[0].p.channels_with(&pk_b).len() == 1 && n[2].p.channels_with(&pk_p).len() == 1
    });

    println!("3. Alice garde presque tout son solde en or.");
    let all = alice.p.balance();
    alice.p.stabilize(all - 1_000, &q0, t).unwrap();
    tick_until(&mut [&mut provider, &mut alice, &mut bruno], &q0, t, 60, "Alice's position", |n| agreed(n[1], n[0]).is_some());
    println!("   Alice : {} MNGR", format_mengers(mengers(&alice, t)));

    // More than 10 % of the channel the provider opened to Bruno: LDK 0.2 refused that much in one
    // payment; since LDK 0.3, a private channel accepts payments as large as itself.
    println!("4. Bruno demande 0,5 MNGR ; Alice paie sa facture, par le fournisseur.");
    let (invoice, sats) = bruno.p.invoice(500_000, &q0, t).unwrap();
    println!("   facture : 0,5 MNGR = {sats} sats");
    assert!(sats > CHANNEL_SATS / 10);
    let alice_sats = alice.p.balance();
    let alice_mengers = mengers(&alice, t);
    alice.p.pay(&invoice, &q0, t).unwrap();
    tick_until(&mut [&mut provider, &mut alice, &mut bruno], &q0, t, 60, "payment, then both positions", |n| {
        // What is left of Alice's margin for fees waits until there is enough to stabilize.
        n[1].p.state.spend.is_none()
            && n[1].p.state.to_stabilize_sats < MIN_ADJUST_SATS
            && n[2].p.state.to_stabilize_sats == 0
            && agreed(n[1], n[0]).is_some()
            && agreed(n[2], n[0]).is_some()
    });
    let paid = alice_sats - alice.p.balance();
    let spent_mengers = alice_mengers - mengers(&alice, t);
    println!(
        "   Alice a payé {paid} sats ; ses Mengers : -{} ; Bruno : {} MNGR",
        format_mengers(spent_mengers),
        format_mengers(mengers(&bruno, t))
    );
    assert!(paid >= sats && paid <= sats + 20, "paid {paid} for an invoice of {sats}");
    assert!(spent_mengers.abs_diff(500_000) <= 5_000, "Alice spent {spent_mengers} µMNGR");
    assert!(mengers(&bruno, t).abs_diff(500_000) <= 5, "Bruno holds {} µMNGR", mengers(&bruno, t));

    println!("5. Le bitcoin baisse de 10 % : le fournisseur complète chez les deux.");
    let t1 = t + 600;
    let q1 = quotes(90_000.0, Some(4_319.0), t1);
    let p1 = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
    tick_until(&mut [&mut provider, &mut alice, &mut bruno], &q1, t1, 60, "both at par", |n| {
        at_par(n[1], n[0], p1, t1) && at_par(n[2], n[0], p1, t1)
    });

    println!("6. Les prix diffèrent de 0,12 % d'un côté à l'autre : le fournisseur complète une fois, personne ne renvoie la différence.");
    let t2 = t1 + 120;
    let savers_view = quotes(90_000.0, Some(4_319.0), t2);
    let provider_view = quotes(90_000.0 / 1.0012, Some(4_319.0), t2);
    let mut sent = [0usize; 3];
    for _ in 0..20 {
        // No pause between payments in these tests: a back-and-forth would show at once.
        for (i, (peer, q)) in
            [(&mut provider, &provider_view), (&mut alice, &savers_view), (&mut bruno, &savers_view)].into_iter().enumerate()
        {
            peer.p.sync();
            for e in peer.p.tick(q, t2).unwrap().events {
                println!("   [{}] {e}", peer.name);
                sent[i] += usize::from(e.starts_with("paiement de stabilité envoyé"));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(sent[0] <= 2 && sent[1] == 0 && sent[2] == 0, "stability payments sent: {sent:?}");
    assert!(agreed(&alice, &provider).is_some() && agreed(&bruno, &provider).is_some(), "ledgers agree");

    println!("7. Bruno, qui n'a jamais financé de canal, rend 0,2 MNGR à Alice.");
    let (t1, q1) = (t2, quotes(90_000.0, Some(4_319.0), t2));
    let (invoice, _) = alice.p.invoice(200_000, &q1, t1).unwrap();
    let (alice_before, bruno_before) = (mengers(&alice, t1), mengers(&bruno, t1));
    bruno.p.pay(&invoice, &q1, t1).unwrap();
    tick_until(&mut [&mut provider, &mut alice, &mut bruno], &q1, t1, 60, "payment back", |n| {
        n[2].p.state.spend.is_none()
            && n[1].p.state.to_stabilize_sats == 0
            && mengers(n[1], t1) > alice_before
            && at_par(n[1], n[0], p1, t1)
            && at_par(n[2], n[0], p1, t1)
    });
    let (alice_after, bruno_after) = (mengers(&alice, t1), mengers(&bruno, t1));
    println!("   Alice : {} MNGR ; Bruno : {} MNGR", format_mengers(alice_after), format_mengers(bruno_after));
    let received = alice_after - alice_before;
    assert!(received.abs_diff(200_000) <= 50, "Alice received {received}");
    assert!((bruno_before - bruno_after).abs_diff(200_000) <= 2_500, "Bruno spent {}", bruno_before - bruno_after);

    for x in [provider, alice, bruno] {
        x.p.stop().unwrap();
    }
}

#[test]
fn a_provider_that_goes_away_and_a_saver_that_leaves() {
    let Some(exe) = bitcoind_exe() else { return };
    let dir = tempfile::tempdir().unwrap();
    let bitcoind = Bitcoind::start(&exe, &dir.path().join("bitcoind")).expect("bitcoind");
    let mut provider = start_with("fournisseur", Role::Provider, dir.path(), &bitcoind, None, |cfg| {
        cfg.return_channel = Some(ReturnChannel { ratio_ppm: 1_000_000, min_sats: 100_000, max_sats: 5_000_000 });
    });
    let mut saver = start("epargnant", Role::Saver, dir.path(), &bitcoind, Some(&provider));
    let (pk_s, pk_p) = (saver.p.node_id(), provider.p.node_id());
    let t = WED_NOON;
    let q0 = quotes(100_000.0, Some(4_319.0), t);
    let p0 = SatsPerMenger::from_usd(100_000.0, 4_319.0).unwrap();

    println!("1. L'épargnant ouvre un canal ; le fournisseur en ouvre un en retour, de lui-même.");
    // Every coin the two of them own is spendable before anything starts, and every later block
    // pays someone else: from here on, their on-chain balances move only with their own
    // transactions.
    let miner = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";
    bitcoind.mine(2, &saver.p.new_address().unwrap()).unwrap();
    bitcoind.mine(2, &provider.p.new_address().unwrap()).unwrap();
    bitcoind.mine(101, miner).unwrap();
    wait_for(120, "on-chain funds", || {
        saver.p.sync();
        provider.p.sync();
        saver.p.onchain_spendable_sats() == 100 * 100_000_000 && provider.p.onchain_spendable_sats() == 100 * 100_000_000
    });
    let open_both_ways = |saver: &mut Peer, provider: &mut Peer, sats: u64| {
        saver.p.open_channel(pk_p, &provider.addr, sats).unwrap();
        wait_for(60, "the saver's funding transaction", || bitcoind.mempool_size().unwrap_or(0) >= 1);
        bitcoind.mine(6, miner).unwrap();
        tick_until(&mut [&mut *saver, &mut *provider], &q0, t, 120, "the channel in return", |_| {
            bitcoind.mempool_size().unwrap_or(0) >= 1
        });
        bitcoind.mine(6, miner).unwrap();
        tick_until(&mut [&mut *saver, &mut *provider], &q0, t, 120, "two usable channels", |n| {
            n[0].p.channels_with(&pk_p).len() == 2 && n[1].p.channels_with(&pk_s).len() == 2
        });
    };
    open_both_ways(&mut saver, &mut provider, CHANNEL_SATS);
    assert!(said(&provider, "ouvert en retour"));
    assert!(provider.p.our_outbound(&pk_s) > CHANNEL_SATS * 9 / 10, "the provider has bitcoin on its side");

    println!("2. Le fournisseur fixe le prix : une demande trop loin de son prix est refusée, rien ne change.");
    let all = saver.p.balance();
    let dearer = quotes(99_000.0, Some(4_319.0), t); // a Menger costs the provider 1 % more satoshis
    saver.p.stabilize(all - 1_000, &q0, t).unwrap();
    tick_views(&mut [(&mut saver, &q0[..]), (&mut provider, &dearer[..])], t, 60, "a refusal", |n| n[0].p.pending().is_none());
    assert!(said(&saver, "le fournisseur refuse"));
    assert!(saver.p.position().is_none() && provider.p.position_with(&pk_s).is_none());

    println!("3. Aux mêmes prix, la demande passe ; puis l'épargnant reprend 300 000 sats en bitcoin.");
    saver.p.stabilize(all - 1_000, &q0, t + 1).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q0, t + 1, 60, "position agreed", |n| agreed(n[0], n[1]).is_some());
    let opened = saver.p.position().unwrap().clone();
    assert_eq!(opened.micro_at_start, p0.micro_for_floor(all - 1_000), "priced by the provider, rounded in its favour");
    saver.p.release(Some(300_000), &q0, t + 2).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q0, t + 2, 60, "smaller position", |n| {
        agreed(n[0], n[1]).is_some_and(|p| p.backing_sats < opened.backing_sats)
    });
    let smaller = saver.p.position().unwrap().clone();
    assert_eq!(smaller.backing_sats, opened.backing_sats - 300_000);
    assert_eq!(opened.micro_at_start - smaller.micro_at_start, p0.micro_for_ceil(300_000));
    assert!(saver.p.free_sats() >= 300_000);

    println!("4. Un paiement que le fournisseur tarde à préparer est abandonné ; les sats retournent en or.");
    assert!(saver.p.cancel_spend(t).is_err(), "nothing to cancel yet");
    let (invoice, sats) = provider.p.invoice(800_000, &q0, t).unwrap();
    assert!(sats > saver.p.free_sats(), "the payment needs part of the position");
    saver.p.pay(&invoice, &q0, t + 3).unwrap();
    assert!(saver.p.state.spend.as_ref().is_some_and(|s| !s.ready));
    // Only the saver's clock moves: the provider has not looked at the request yet.
    let late = t + 3 + SPEND_TIMEOUT_SECS;
    let q_late = quotes(100_000.0, Some(4_319.0), late);
    saver.p.tick(&q_late, late).unwrap();
    assert!(saver.p.state.spend.is_none() && said(&saver, "paiement abandonné"));
    // The provider answers at last: the satoshis it releases have nowhere to go, so they go back.
    tick_until(&mut [&mut saver, &mut provider], &q_late, late, 60, "satoshis back in the position", |n| {
        n[0].p.pending().is_none()
            && n[0].p.state.to_stabilize_sats < MIN_ADJUST_SATS
            && agreed(n[0], n[1]).is_some_and(|p| p.backing_sats >= smaller.backing_sats - 10)
    });
    assert!(saver.p.state.spend.is_none());
    assert!(said(&provider, "−") && said(&saver, "sats à garder en or : demande envoyée"), "released, then stabilized again");
    let stable = saver.p.position().unwrap().clone();
    let micro = stable.micro_mengers(late);

    println!("5. Le fournisseur s'absente pendant une baisse de 10 % : l'épargnant attend, il ne ferme rien.");
    let provider_cfg = provider.cfg.clone();
    let provider_addr = provider.addr.clone();
    provider.p.stop().unwrap();
    let t1 = late + 600;
    let q1 = quotes(90_000.0, Some(4_319.0), t1);
    let p1 = SatsPerMenger::from_usd(90_000.0, 4_319.0).unwrap();
    wait_for(30, "the saver sees its provider gone", || !saver.p.reachable(&pk_p));
    for later in [0, 11, 3_600] {
        // The hour without a tick is a computer that slept: the provider's delay starts again.
        saver.p.cfg.max_tick_gap_secs = (later == 3_600).then_some(300);
        let r = saver.p.tick(&quotes(90_000.0, Some(4_319.0), t1 + later), t1 + later).unwrap();
        assert!(matches!(action(&r), Action::Await { .. }), "after {later} s: {:?}", action(&r));
    }
    saver.p.cfg.max_tick_gap_secs = None;
    assert!(said(&saver, "les délais repartent de zéro"));
    let seen = saver.p.peer(&pk_p).unwrap();
    assert!(seen.closed.is_none() && seen.unreachable_since == Some(t1 + 3_600));
    assert_eq!(seen.owed_to_us_since, Some(t1 + 3_600));
    assert_eq!(saver.p.channels_all(&pk_p).len(), 2, "both channels are still there");
    assert_eq!(saver.p.position(), Some(&stable));
    assert!(saver.p.stabilize(10_000, &q1, t1).unwrap_err().to_string().contains("injoignable"));
    assert!(saver.p.leave(false, t1).unwrap_err().to_string().contains("injoignable"));

    println!("6. Il revient, avec ce qu'il avait noté : il paie ce qu'il doit.");
    let mut provider =
        Peer { p: Party::start(provider_cfg.clone()).expect("provider back"), addr: provider_addr, name: "fournisseur", cfg: provider_cfg };
    assert_eq!(provider.p.position_with(&pk_s), Some(&stable), "its books survived");
    let _ = saver.p.node.connect(pk_p, provider.addr.parse().unwrap(), true);
    let t2 = t1 + 3_700;
    let q2 = quotes(90_000.0, Some(4_319.0), t2);
    tick_until(&mut [&mut saver, &mut provider], &q2, t2, 120, "the provider pays", |n| at_par(n[0], n[1], p1, t2));
    assert!(saver.p.peer(&pk_p).unwrap().unreachable_since.is_none());

    println!("7. L'épargnant redémarre aussi, puis quitte son fournisseur : ses bitcoins reviennent sur la chaîne.");
    let mut saver = restart(saver);
    let _ = saver.p.node.connect(pk_p, provider.addr.parse().unwrap(), true);
    tick_until(&mut [&mut saver, &mut provider], &q2, t2, 120, "channels usable again", |n| n[0].p.reachable(&pk_p));
    assert!(at_par(&saver, &provider, p1, t2), "nothing was lost in the restart");
    let (in_channels, on_chain) = (saver.p.balance(), saver.p.onchain_spendable_sats());
    assert!(in_channels > p1.sats_for(micro) - 2_000, "the saver holds the value of its Mengers: {in_channels}");
    saver.p.leave(false, t2).unwrap();
    assert!(saver.p.position().is_none() && saver.p.stabilize(10_000, &q2, t2).is_err());
    tick_until(&mut [&mut saver, &mut provider], &q2, t2, 60, "closing transactions", |_| bitcoind.mempool_size().unwrap_or(0) >= 2);
    // Out of the channels and not yet in a block: the saver still sees what is coming back.
    assert!(saver.p.balance() == 0 && saver.p.returning_sats() > in_channels - 5_000, "returning {}", saver.p.returning_sats());
    bitcoind.mine(1, miner).unwrap();
    wait_for(60, "one confirmation is enough after a polite close", || {
        saver.p.sync();
        saver.p.returning_sats() == 0 && saver.p.onchain_spendable_sats() > on_chain + in_channels - 5_000
    });
    bitcoind.mine(5, miner).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q2, t2, 120, "bitcoin back on the chain", |n| {
        n[0].p.channels_all(&pk_p).is_empty()
            && n[1].p.channels_all(&pk_s).is_empty()
            && n[0].p.onchain_spendable_sats() > on_chain + in_channels - 5_000
    });
    assert!(provider.p.position_with(&pk_s).is_none() && provider.p.peer(&pk_s).unwrap().closed.is_some());
    assert!(saver.p.peer(&pk_p).unwrap().closing_since.is_none());

    println!("8. Il retire une partie de ses bitcoins vers une autre adresse.");
    let elsewhere = provider.p.new_address().unwrap();
    assert!(saver.p.withdraw("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", Some(10_000), t2).is_err(), "not an address of this network");
    assert!(saver.p.withdraw("pas une adresse", None, t2).is_err());
    let before = saver.p.onchain_spendable_sats();
    let txid = saver.p.withdraw(&elsewhere, Some(1_000_000), t2).unwrap();
    wait_for(60, "the withdrawal in the mempool", || bitcoind.mempool_size().unwrap_or(0) >= 1);
    let tx = bitcoind.rpc("getrawtransaction", serde_json::json!([txid, true])).unwrap();
    let pays = |v: &serde_json::Value| v["scriptPubKey"]["address"] == elsewhere.as_str() && v["value"].as_f64() == Some(0.01);
    assert!(tx["vout"].as_array().unwrap().iter().any(pays), "0.01 BTC to the address asked: {tx}");
    bitcoind.mine(1, miner).unwrap();
    wait_for(60, "the withdrawal leaves the wallet", || {
        saver.p.sync();
        let now = saver.p.onchain_spendable_sats();
        now < before - 1_000_000 && now > before - 1_010_000
    });

    println!("9. Il revient chez le même fournisseur : nouveaux canaux, nouvelle position.");
    let t3 = t2 + 600;
    let q3 = quotes(90_000.0, Some(4_319.0), t3);
    open_both_ways(&mut saver, &mut provider, CHANNEL_SATS / 2);
    assert!(saver.p.peer(&pk_p).unwrap().closed.is_none() && provider.p.peer(&pk_s).unwrap().closed.is_none());
    saver.p.stabilize(500_000, &q3, t3).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q3, t3, 60, "a new position", |n| agreed(n[0], n[1]).is_some());
    let again = saver.p.position().unwrap().clone();
    assert_eq!(again.backing_sats, 500_000);

    println!("10. Le fournisseur disparaît pour de bon pendant un krach : après six heures, l'épargnant sort de force.");
    provider.p.stop().unwrap();
    wait_for(30, "the saver sees its provider gone", || !saver.p.reachable(&pk_p));
    let t4 = t3 + 600;
    let q4 = quotes(75_000.0, Some(4_319.0), t4);
    let r = saver.p.tick(&q4, t4).unwrap();
    assert!(matches!(action(&r), Action::Await { .. }));
    let almost = t4 + 6 * 3_600 - 1;
    let r = saver.p.tick(&quotes(75_000.0, Some(4_319.0), almost), almost).unwrap();
    assert!(matches!(action(&r), Action::Await { .. }), "one second before the end of the delay");
    let (in_channels, on_chain) = (saver.p.balance(), saver.p.onchain_spendable_sats());
    let t5 = t4 + 6 * 3_600;
    let q5 = quotes(75_000.0, Some(4_319.0), t5);
    let r = saver.p.tick(&q5, t5).unwrap();
    assert!(matches!(action(&r), Action::Close { reason: CloseReason::Unpaid { .. } }));
    let seen = saver.p.peer(&pk_p).unwrap();
    assert!(seen.closed.is_some() && seen.force_closed && saver.p.position().is_none());
    wait_for(60, "the saver's own closing transactions", || bitcoind.mempool_size().unwrap_or(0) >= 1);
    // What the saver holds is locked for the channel's waiting period (144 blocks), then swept.
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut waited = false;
    loop {
        bitcoind.mine(20, miner).unwrap();
        saver.p.sync();
        let _ = saver.p.tick(&q5, t5 + 1);
        waited |= saver.p.returning_sats() > 0;
        let back = saver.p.onchain_spendable_sats().saturating_sub(on_chain);
        if back > in_channels - 20_000 && saver.p.returning_sats() == 0 {
            assert!(back <= in_channels + 60_000, "its balance and the fee reserve of its two channels, nothing else: {back}");
            println!("   revenu sur la chaîne : {back} sats, pour {in_channels} détenus dans les canaux");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the saver's bitcoin never came back: {back} of {in_channels}, {} still returning",
            saver.p.returning_sats()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(waited, "the bitcoin was seen on its way back before it arrived");
    saver.p.stop().unwrap();
}

#[test]
fn a_saver_that_moves_its_bitcoin_away_is_not_paid() {
    let Some(exe) = bitcoind_exe() else { return };
    let dir = tempfile::tempdir().unwrap();
    let bitcoind = Bitcoind::start(&exe, &dir.path().join("bitcoind")).expect("bitcoind");
    let mut provider = start("fournisseur", Role::Provider, dir.path(), &bitcoind, None);
    let mut saver = start("epargnant", Role::Saver, dir.path(), &bitcoind, Some(&provider));
    let (pk_s, pk_p) = (saver.p.node_id(), provider.p.node_id());
    let t = WED_NOON;
    let q0 = quotes(100_000.0, Some(4_319.0), t);

    bitcoind.mine(101, &saver.p.new_address().unwrap()).unwrap();
    bitcoind.mine(101, &provider.p.new_address().unwrap()).unwrap();
    wait_for(120, "on-chain funds", || {
        saver.p.sync();
        provider.p.sync();
        saver.p.onchain_spendable_sats() > 5 * CHANNEL_SATS && provider.p.onchain_spendable_sats() > 5 * CHANNEL_SATS
    });
    saver.p.open_channel(pk_p, &provider.addr, CHANNEL_SATS).unwrap();
    provider.p.open_channel(pk_s, &saver.addr, CHANNEL_SATS).unwrap();
    wait_for(60, "funding transactions", || bitcoind.mempool_size().unwrap_or(0) >= 2);
    bitcoind.mine(6, &provider.p.new_address().unwrap()).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q0, t, 120, "two usable channels", |n| {
        n[0].p.channels_with(&pk_p).len() == 2 && n[1].p.channels_with(&pk_s).len() == 2
    });

    println!("1. L'épargnant garde 1 000 000 sats en or : les deux côtés comptent le même solde.");
    saver.p.stabilize(1_000_000, &q0, t).unwrap();
    tick_until(&mut [&mut saver, &mut provider], &q0, t, 60, "position agreed", |n| agreed(n[0], n[1]).is_some());
    // The provider never counts less than the saver counts for itself: an honest saver is never
    // taken for one that moved its bitcoin away. (The two differ by the small outputs that a
    // channel's opener sets aside for fees.)
    let (own, seen) = (saver.p.balance(), provider.p.their_balance(&pk_s));
    assert!(seen >= own && seen - own < 1_500, "the saver counts {own}, the provider sees {seen}");

    println!("2. Un épargnant malhonnête sort ses sats du canal sans toucher à sa position.");
    // Not something the program does: a modified one could. The payment is an ordinary one.
    saver.p.node.spontaneous_payment().send(1_500_000_000, pk_p, None).unwrap();
    tick_until(&mut [&mut provider], &q0, t, 60, "the provider sees the saver's balance fall", |n| {
        n[0].p.their_balance(&pk_s) < 600_000 && !n[0].p.payments_under_way(&pk_s)
    });
    assert_eq!(provider.p.position_with(&pk_s).unwrap().backing_sats, 1_000_000, "the position still counts them");

    println!("3. Le bitcoin baisse de 10 % : le fournisseur ne paie pas une position qui n'est plus garantie, puis ferme.");
    // The delay (ten seconds in these tests) runs from the moment the provider saw it, at `t`.
    let t1 = t + 5;
    let r = provider.p.tick(&quotes(90_000.0, Some(4_319.0), t1), t1).unwrap();
    assert!(matches!(action(&r), Action::Await { owed_sats, since } if *owed_sats > 400_000 && *since == t), "{:?}", action(&r));
    assert!(!r.events.iter().any(|e| e.contains("paiement de stabilité envoyé")));
    let t2 = t + 11;
    let r = provider.p.tick(&quotes(90_000.0, Some(4_319.0), t2), t2).unwrap();
    assert!(matches!(action(&r), Action::Close { reason: CloseReason::BackingMissing { .. } }), "{:?}", action(&r));
    assert!(provider.p.peer(&pk_s).unwrap().closed.is_some() && provider.p.position_with(&pk_s).is_none());

    saver.p.stop().unwrap();
    provider.p.stop().unwrap();
}
