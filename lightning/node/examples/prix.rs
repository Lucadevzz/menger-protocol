//! Lit les sources de prix publiques et affiche le prix du Menger : `cargo run --example prix`.

use menger_core::price::{menger_price, PriceRules};
use menger_core::unit::format_mengers;
use menger_ln::prices;

fn main() {
    let now = chrono::Utc::now().timestamp();
    let (quotes, errors) = prices::read_all(&prices::live_sources(), now);
    for q in &quotes {
        println!("{:<16} {:?}  {:>12.2} $  (il y a {} s)", q.source, q.asset, q.usd, now - q.at);
    }
    for e in &errors {
        println!("indisponible : {e}");
    }
    let mut gold = None;
    match menger_price(&quotes, now, &PriceRules::default(), &mut gold) {
        Ok(p) => {
            let usd = p.xau_usd * menger_core::unit::GRAMS_PER_MENGER / menger_core::unit::GRAMS_PER_TROY_OUNCE;
            println!(
                "\n1 Menger (3,545 g d'or) = {:.2} $ = {} sats{}",
                usd,
                p.sats_per_menger.0,
                if p.gold_carried { " (or du vendredi)" } else { "" }
            );
            println!("100 000 sats = {} MNGR", format_mengers(p.sats_per_menger.micro_for(100_000)));
        }
        Err(e) => println!("\npas de prix fiable : {e}"),
    }
}
