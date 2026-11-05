# Menger : mémoire du projet

Projet de Matteo (GitHub Lucadevzz) : une monnaie privée indexée sur l'or, **1 Menger (MNGR) = 3,545 g d'or fin**. L'objectif est la version la plus décentralisée possible, celle qui aurait plu à Menger, Mises et Hayek. Matteo n'est pas développeur : il faut lui expliquer simplement, en français, avec des réponses courtes.

## Deux versions dans ce dépôt

- **Racine : v0 sur Ethereum.** Fork de Liquity V1 (Solidity 0.6.11, Foundry). L'unité de dette est le Menger, la garantie est l'ETH. Il n'y a pas de jeton LQTY : les frais vont au pool de stabilité. Les changements par rapport à Liquity sont marqués `MENGER:` et le diff est dans `audit/liquity-v1-to-menger.diff`. Le rapport pour les auditeurs est `audit/AUDIT.md`, le guide de déploiement `DEPLOIEMENT.md`.
  - **Démo en ligne** : https://menger.money (GitHub Pages depuis `main`, dossier `/`, fichier `CNAME`). Le domaine est chez Cloudflare, en DNS seulement (nuage gris).
  - **Sepolia**, déployé le 4 novembre 2026 : les adresses sont dans `deployments/11155111.json`. Le déployeur `0xe0cE…9f69` est le compte de Matteo et contrôle les 4 sources de prix de test. Les rachats sont bloqués jusqu'au 18 novembre 2026 (période de démarrage de Liquity).
  - Pour les scripts de démo sur Sepolia, utiliser `--gas-estimate-multiplier 500`. Le déploiement, lui, se fait avec 200.
- **`lightning/` : version Bitcoin.** C'est la direction choisie le 4 novembre 2026. Des canaux stables en or sur Lightning (idée des Stable Channels, GPL v3), en Rust : `core` contient les règles pures, `node` le nœud (LDK Node 0.8 en version de développement, voir Décisions), le programme `menger-ln` et les tests de bout en bout `node/tests/regtest.rs`. Voir `lightning/README.md`.
  - L'application de l'épargnant est une page web servie par son nœud sur 127.0.0.1:8421 (`node/src/interface.html`, `web.rs`, `api.rs`). Protections : écoute locale seulement, contrôle de l'en-tête `Host`, secret par lancement dans l'en-tête `x-menger-jeton`, page interdite dans un cadre, texte venu du réseau toujours inséré en texte.
  - `menger-ln essai --bitcoind CHEMIN` : réseau privé avec fournisseur (8420), Alice (8421) et Bruno (8422), prix fictifs modifiables depuis la page, un bloc toutes les 10 secondes.
  - Clés : phrase de récupération de 12 mots (BIP 39) dans `phrase-de-recuperation.txt` du dossier de données (droits 600) ; les nœuds créés avant gardent `ldk/keys_seed` ; `--restaurer` lit la phrase au démarrage.
  - Un fournisseur sert plusieurs épargnants. Une position change par demandes signées (`Adjust`, puis `Reply`), en plus ou en moins, au prix de l'épargnant vérifié par le fournisseur. Une demande sans réponse est renvoyée et le fournisseur redonne la même réponse.
  - Depuis le 5 novembre 2026, un épargnant paie une facture Lightning depuis son solde (la position est réduite d'abord) et ce qu'il reçoit est stabilisé automatiquement. Le paiement passe par le fournisseur.

## Décisions

- **Unité or plutôt que dollar.** Un Menger en dollars ferait doublon avec LUSD et ses copies.
- **Nom.** Ducat, puis Catallax, puis **Menger** (choisi le 3 novembre 2026). Ducat Protocol occupe « Ducat », et un protocole Nostr s'appelle déjà Catallax.
- **Bitcoin plutôt qu'Ethereum** pour la version finale. Les canaux Lightning ne demandent ni jeton, ni pont, ni oracle commun : chaque partie lit ses propres prix. Les covenants ne sont pas activés sur Bitcoin. Citrea a été écarté à cause de son conseil de sécurité, et le WBTC parce qu'il dépend d'un dépositaire.
- **Prix de l'or** : Swissquote (au comptant) et PAXG sur Kraken, qui doivent concorder à 1 % près. Le prix de l'or est gelé du vendredi 21 h au lundi 0 h UTC.
- **Le fournisseur se déclare service LSPS2** : avec LDK Node, c'est ce qui lui permet de transmettre des paiements vers les canaux privés des épargnants. Les épargnants ne sont **pas** clients LSPS2 : ils n'acceptent jamais un canal non confirmé (pas de confiance).
- **LDK Node 0.8 en version de développement** (dépôt git, commit `79e4d92`, 29 octobre 2026), choisi par Matteo le 5 novembre 2026 : avec LDK 0.3, un canal privé accepte des paiements aussi gros que lui (avec LDK Node 0.7, c'était 10 % du canal ouvert par l'autre partie). Passer à la 0.8 officielle dès sa sortie, et avant tout vrai bitcoin.
- **Délai de punition** : 144 blocs par défaut, donc l'épargnant doit se connecter au moins une fois par jour. LDK permet deux semaines, LDK Node ne l'expose pas : contribution à proposer aux auteurs de LDK Node (à faire au nom de Matteo seulement avec son accord).
- **Stabilisation automatique** de ce qu'un épargnant reçoit : elle attend qu'aucun paiement ne soit en cours dans ses canaux (sinon le solde n'est pas encore à jour) ; si le fournisseur refuse, les sats sont reproposés 5 minutes plus tard.
- **Une ouverture de canal à la fois** par portefeuille : LDK Node ne réserve pas les pièces d'une transaction d'ouverture, et deux ouvertures rapprochées ont dépensé la même pièce (vu le 5 novembre 2026). `open_channel` refuse tant que la précédente n'est pas connue du portefeuille ou confirmée.
- **Licence GPL v3** partout.

## Commandes

```bash
forge test                                               # v0 : 71 tests
cd lightning && cargo test --workspace                   # règles, sources, clés, application (34 + 13 tests)
BITCOIND_EXE=/chemin/bitcoind cargo test --workspace     # + les 2 tests de bout en bout (regtest)
cargo run --release -- essai --bitcoind /chemin/bitcoind # l'application dans le navigateur, prix fictifs
cargo run --release -- demo --bitcoind /chemin/bitcoind  # démonstration commentée
cargo run --release --example prix                       # prix réels
```

Dans le bac à sable de Claude, `bitcoind` n'est pas installé : il a été compilé depuis les sources (v30.4, sans portefeuille). Les requêtes HTTP passent par le proxy défini dans `HTTPS_PROXY`. Le disque du bac à sable est limité : `cargo clean` si la place manque (les dépendances sont compilées sans informations de débogage pour rester petites).

## Prochaines étapes

1. Faire tourner `menger-ln` sur Mutinynet. Le robinet refuse le compte GitHub de Matteo (5 novembre 2026) : il faut des sats Mutinynet d'une autre façon, par exemple auprès d'un développeur.
2. Une application mobile pour l'épargnant (la page web locale existe depuis le 5 novembre 2026).
3. Une relecture par un développeur Bitcoin et Lightning, puis un audit avant tout usage de vrais BTC.
4. Faire tester la démo Ethereum à 20 ou 50 personnes et demander aux bitcoiners s'ils emprunteraient de l'or contre leurs BTC.

## Règles de travail

- Le texte destiné à Matteo et aux utilisateurs est en français, avec les espaces typographiques. Le code et les commentaires sont en anglais.
- Tous les tests doivent passer avant de pousser sur `main`.
- Ne jamais mettre de clé privée ni de secret dans le dépôt.
