//! What the interface's buttons do: a small JSON API over one party.

use crate::party::{Party, Report};
use crate::web::{ApiCall, ApiReply};
use anyhow::{anyhow, bail, Result};
use ldk_node::bitcoin::Network;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescriptionRef};
use menger_core::price::Quote;
use menger_core::stability::Role;
use menger_core::unit::{grams, parse_mengers};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;

/// The trial on a private network: a market whose prices the person moves from the page.
pub struct Sandbox {
    pub btc_usd: f64,
    pub xau_usd: f64,
    /// The other people of the trial, with the address of their page.
    pub others: Vec<(String, String)>,
}

pub struct ApiContext<'a> {
    /// The last tick, for the price shown.
    pub last: Option<&'a Report>,
    /// Fresh quotes, for an action that needs a price.
    pub quotes: &'a dyn Fn() -> Vec<Quote>,
    pub now: i64,
    pub network: Network,
    /// A first name, in the trial.
    pub name: &'a str,
    pub sandbox: Option<&'a Sandbox>,
}

pub fn handle(party: &mut Party, call: &ApiCall, ctx: &ApiContext) -> ApiReply {
    match route(party, call, ctx) {
        Ok(value) => ApiReply::ok(value),
        Err(e) => ApiReply::error(400, format!("{e:#}")),
    }
}

fn body<T: DeserializeOwned>(call: &ApiCall) -> Result<T> {
    serde_json::from_str(&call.body).map_err(|e| anyhow!("requête invalide : {e}"))
}

fn route(party: &mut Party, call: &ApiCall, ctx: &ApiContext) -> Result<Value> {
    match (call.method.as_str(), call.path.as_str()) {
        ("GET", "/api/etat") => Ok(state(party, ctx)),
        ("POST", "/api/stabiliser") => {
            #[derive(Deserialize)]
            struct Body {
                sats: Option<u64>,
                #[serde(default)]
                tout: bool,
            }
            let b: Body = body(call)?;
            let sats = if b.tout { party.free_sats() } else { b.sats.ok_or_else(|| anyhow!("montant manquant"))? };
            if sats == 0 {
                bail!("aucun sat libre à garder en or");
            }
            party.stabilize(sats, &(ctx.quotes)(), ctx.now)?;
            Ok(json!({ "sats": sats }))
        }
        ("POST", "/api/facture") => {
            #[derive(Deserialize)]
            struct Body {
                mengers: String,
            }
            let b: Body = body(call)?;
            let micro = parse_mengers(&b.mengers).ok_or_else(|| anyhow!("montant en Mengers attendu, par exemple 0,5"))?;
            if micro == 0 {
                bail!("le montant doit être plus grand que zéro");
            }
            let (invoice, sats) = party.invoice(micro, &(ctx.quotes)(), ctx.now)?;
            let qr = qr_svg(&invoice.to_uppercase())?;
            Ok(json!({ "facture": invoice, "sats": sats, "micro": micro, "qr": qr }))
        }
        ("POST", "/api/decoder") => {
            #[derive(Deserialize)]
            struct Body {
                facture: String,
            }
            let b: Body = body(call)?;
            let text = b.facture.trim();
            let text = text.strip_prefix("lightning:").or_else(|| text.strip_prefix("LIGHTNING:")).unwrap_or(text);
            let invoice = Bolt11Invoice::from_str(text).map_err(|e| anyhow!("ce n'est pas une facture Lightning : {e}"))?;
            let sats = invoice.amount_milli_satoshis().map(|m| m.div_ceil(1000));
            let price = ctx.last.and_then(|r| r.price.as_ref());
            let micro = sats.zip(price).map(|(s, p)| p.sats_per_menger.micro_for(s));
            let description = match invoice.description() {
                Bolt11InvoiceDescriptionRef::Direct(d) => d.to_string(),
                Bolt11InvoiceDescriptionRef::Hash(_) => String::new(),
            };
            Ok(json!({ "facture": text, "sats": sats, "micro": micro, "description": description, "expiree": invoice.is_expired() }))
        }
        ("POST", "/api/payer") => {
            #[derive(Deserialize)]
            struct Body {
                facture: String,
            }
            let b: Body = body(call)?;
            let text = b.facture.trim();
            let text = text.strip_prefix("lightning:").or_else(|| text.strip_prefix("LIGHTNING:")).unwrap_or(text);
            party.pay(text, &(ctx.quotes)(), ctx.now)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/adresse") => {
            let address = party.new_address()?;
            let qr = qr_svg(&format!("bitcoin:{address}").to_uppercase())?;
            Ok(json!({ "adresse": address, "qr": qr }))
        }
        ("POST", "/api/canal") => {
            #[derive(Deserialize)]
            struct Body {
                sats: u64,
            }
            let b: Body = body(call)?;
            let (key, address) =
                party.cfg.provider.clone().ok_or_else(|| anyhow!("adresse du fournisseur inconnue : lance le nœud avec --fournisseur"))?;
            party.open_channel(key, &address, b.sats)?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(anyhow!("action inconnue : {} {}", call.method, call.path)),
    }
}

fn qr_svg(data: &str) -> Result<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(data.as_bytes()).map_err(|e| anyhow!("QR : {e}"))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .dark_color(svg::Color("#111111"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

fn network_label(network: Network) -> &'static str {
    match network {
        Network::Regtest => "Réseau privé d'essai",
        Network::Signet => "Signet, réseau de test",
        Network::Testnet | Network::Testnet4 => "Testnet, réseau de test",
        _ => "Bitcoin : vrais bitcoins",
    }
}

/// Everything the page shows.
pub fn state(party: &Party, ctx: &ApiContext) -> Value {
    let price = ctx.last.and_then(|r| r.price.as_ref());
    let journal: Vec<Value> =
        party.state.journal.iter().rev().take(30).map(|e| json!({ "at": e.at, "texte": e.text })).collect();
    let mut v = json!({
        "role": if party.role() == Role::Saver { "epargnant" } else { "fournisseur" },
        "nom": ctx.name,
        "reseau": network_label(ctx.network),
        "vrais_bitcoins": ctx.network == Network::Bitcoin,
        "noeud": party.node_id().to_string(),
        "maintenant": ctx.now,
        "prix": price.map(|p| json!({
            "sats_par_menger": p.sats_per_menger.0,
            "btc_usd": p.btc_usd,
            "or_usd": p.xau_usd,
            "or_du_vendredi": p.gold_carried,
        })),
        "prix_erreur": ctx.last.and_then(|r| r.price_error.clone()),
        "chaine_sats": party.onchain_spendable_sats(),
        "journal": journal,
        "essai": ctx.sandbox.map(|s| json!({
            "btc_usd": s.btc_usd,
            "or_usd": s.xau_usd,
            "autres": s.others.iter().map(|(name, url)| json!({ "nom": name, "url": url })).collect::<Vec<_>>(),
        })),
    });
    let extra = match party.role() {
        Role::Saver => {
            let provider = party.provider().ok();
            let channels: Vec<_> = provider
                .map(|p| party.node.list_channels().into_iter().filter(|c| c.counterparty.node_id == p).collect())
                .unwrap_or_default();
            let ready = channels.iter().filter(|c| c.is_usable).count();
            let position = party.position();
            json!({
                "solde_sats": party.balance(),
                "libres_sats": party.free_sats(),
                "a_stabiliser_sats": party.state.to_stabilize_sats,
                "stable": position.map(|p| {
                    let micro = p.micro_mengers(ctx.now);
                    json!({
                        "micro": micro,
                        "grammes": grams(micro),
                        "sats": p.backing_sats,
                        "valeur_sats": price.map(|pr| pr.sats_per_menger.sats_for(micro)),
                    })
                }),
                "canaux": { "prets": ready, "en_attente": channels.len() - ready },
                "fournisseur": provider.map(|p| p.to_string()),
                "peut_ouvrir_canal": party.cfg.provider.is_some(),
                "demande": party.pending().map(|p| json!({ "sats": p.delta_backing, "micro": p.delta_micro })),
                "paiement": party.state.spend.as_ref().map(|s| json!({ "sats": s.sats, "pret": s.ready, "envoye": s.payment_id.is_some() })),
                "ferme": provider.and_then(|p| party.peer(&p)).and_then(|p| p.closed.clone()),
            })
        }
        Role::Provider => {
            let savers: Vec<Value> = party
                .state
                .peers
                .iter()
                .filter_map(|(key, peer)| {
                    let position = peer.position.as_ref()?;
                    let id = ldk_node::bitcoin::secp256k1::PublicKey::from_str(key).ok()?;
                    let micro = position.micro_mengers(ctx.now);
                    Some(json!({
                        "noeud": key,
                        "micro": micro,
                        "grammes": grams(micro),
                        "sats": position.backing_sats,
                        "notre_cote_sats": party.our_outbound(&id),
                        "ferme": peer.closed,
                    }))
                })
                .collect();
            json!({ "epargnants": savers })
        }
    };
    if let (Some(obj), Value::Object(more)) = (v.as_object_mut(), extra) {
        obj.extend(more);
    }
    v
}
