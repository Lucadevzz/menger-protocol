//! End to end on a private Bitcoin network: real Lightning nodes, real channels, real payments.
//! Needs a `bitcoind` binary: `BITCOIND_EXE=/path/to/bitcoind cargo test -- --nocapture`.
//! Without it, the tests say so and pass.

use ldk_node::bitcoin::Network;
use menger_core::price::{Asset, Quote};
use menger_core::stability::{Action, CloseReason, Params, Position, Role};
use menger_core::unit::{format_mengers, SatsPerMenger};
use menger_ln::party::MIN_ADJUST_SATS;
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
}

fn start(name: &'static str, role: Role, dir: &Path, b: &Bitcoind, provider: Option<&Peer>) -> Peer {
    let addr = format!("127.0.0.1:{}", free_port());
    let chain = ChainSource::BitcoindRpc {
        host: "127.0.0.1".into(),
        port: b.rpc_port,
        user: b.user.clone(),
        password: b.password.clone(),
    };
    let mut cfg = PartyConfig::new(role, dir.join(name), Network::Regtest, addr.clone(), chain);
    // Fast rules for the test: no pause between payments, ten seconds of grace.
    cfg.params = Params { cooldown_secs: 0, grace_secs: 10, ..Params::default() };
    cfg.provider = provider.map(|p| (p.p.node_id(), p.addr.clone()));
    Peer { p: Party::start(cfg).expect("node starts"), addr, name }
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

    // A saver accepts at most 10 % of a channel the provider opened to it in one payment.
    println!("4. Bruno demande 0,3 MNGR ; Alice paie sa facture, par le fournisseur.");
    let (invoice, sats) = bruno.p.invoice(300_000, &q0, t).unwrap();
    println!("   facture : 0,3 MNGR = {sats} sats");
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
    assert!(spent_mengers.abs_diff(300_000) <= 3_000, "Alice spent {spent_mengers} µMNGR");
    assert!(mengers(&bruno, t).abs_diff(300_000) <= 5, "Bruno holds {} µMNGR", mengers(&bruno, t));

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
    // Alice's leftover margin from step 4 joined her position with this payment.
    let received = alice_after - alice_before;
    assert!((200_000..=202_000).contains(&received), "Alice received {received}");
    assert!((bruno_before - bruno_after).abs_diff(200_000) <= 2_500, "Bruno spent {}", bruno_before - bruno_after);

    for x in [provider, alice, bruno] {
        x.p.stop().unwrap();
    }
}
