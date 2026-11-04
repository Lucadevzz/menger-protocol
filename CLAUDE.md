# Menger : mémoire du projet

Projet de Matteo (GitHub Lucadevzz) : une monnaie privée indexée sur l'or, **1 Menger (MNGR) = 3,545 g d'or fin**. L'objectif est la version la plus décentralisée possible, celle qui aurait plu à Menger, Mises et Hayek. Matteo n'est pas développeur : il faut lui expliquer simplement, en français, avec des réponses courtes.

## Deux versions dans ce dépôt

- **Racine : v0 sur Ethereum.** Fork de Liquity V1 (Solidity 0.6.11, Foundry). L'unité de dette est le Menger, la garantie est l'ETH. Il n'y a pas de jeton LQTY : les frais vont au pool de stabilité. Les changements par rapport à Liquity sont marqués `MENGER:` et le diff est dans `audit/liquity-v1-to-menger.diff`. Le rapport pour les auditeurs est `audit/AUDIT.md`, le guide de déploiement `DEPLOIEMENT.md`.
  - **Démo en ligne** : https://menger.money (GitHub Pages depuis `main`, dossier `/`, fichier `CNAME`). Le domaine est chez Cloudflare, en DNS seulement (nuage gris).
  - **Sepolia**, déployé le 4 novembre 2026 : les adresses sont dans `deployments/11155111.json`. Le déployeur `0xe0cE…9f69` est le compte de Matteo et contrôle les 4 sources de prix de test. Les rachats sont bloqués jusqu'au 18 novembre 2026 (période de démarrage de Liquity).
  - Pour les scripts de démo sur Sepolia, utiliser `--gas-estimate-multiplier 500`. Le déploiement, lui, se fait avec 200.
- **`lightning/` : version Bitcoin.** C'est la direction choisie le 4 novembre 2026. Un canal stable en or sur Lightning (idée des Stable Channels, GPL v3), en Rust : `core` contient les règles pures, `node` le nœud LDK Node 0.7, le programme `menger-ln` et le test de bout en bout `node/tests/regtest.rs`. Voir `lightning/README.md`.

## Décisions

- **Unité or plutôt que dollar.** Un Menger en dollars ferait doublon avec LUSD et ses copies.
- **Nom.** Ducat, puis Catallax, puis **Menger** (choisi le 3 novembre 2026). Ducat Protocol occupe « Ducat », et un protocole Nostr s'appelle déjà Catallax.
- **Bitcoin plutôt qu'Ethereum** pour la version finale. Les canaux Lightning ne demandent ni jeton, ni pont, ni oracle commun : chaque partie lit ses propres prix. Les covenants ne sont pas activés sur Bitcoin. Citrea a été écarté à cause de son conseil de sécurité, et le WBTC parce qu'il dépend d'un dépositaire.
- **Prix de l'or** : Swissquote (au comptant) et PAXG sur Kraken, qui doivent concorder à 1 % près. Le prix de l'or est gelé du vendredi 21 h au lundi 0 h UTC.
- **Licence GPL v3** partout.

## Commandes

```bash
forge test                                               # v0 : 71 tests
cd lightning && cargo test --workspace                   # règles et sources
BITCOIND_EXE=/chemin/bitcoind cargo test --workspace     # + bout en bout (regtest)
cargo run --release -- demo --bitcoind /chemin/bitcoind  # démonstration commentée
cargo run --release --example prix                       # prix réels
```

Dans le bac à sable de Claude, `bitcoind` n'est pas installé : il a été compilé depuis les sources (v30.4, sans portefeuille). Les requêtes HTTP passent par le proxy défini dans `HTTPS_PROXY`.

## Prochaines étapes

1. Faire tourner `menger-ln` sur Mutinynet, avec les sats du robinet.
2. Permettre de payer un tiers en Mengers, en faisant passer les paiements par le fournisseur.
3. Une application simple pour l'épargnant.
4. Une relecture par un développeur Bitcoin et Lightning, puis un audit avant tout usage de vrais BTC.
5. Faire tester la démo Ethereum à 20 ou 50 personnes et demander aux bitcoiners s'ils emprunteraient de l'or contre leurs BTC.

## Règles de travail

- Le texte destiné à Matteo et aux utilisateurs est en français, avec les espaces typographiques. Le code et les commentaires sont en anglais.
- Tous les tests doivent passer avant de pousser sur `main`.
- Ne jamais mettre de clé privée ni de secret dans le dépôt.
