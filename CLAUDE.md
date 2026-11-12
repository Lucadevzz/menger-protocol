# Menger : mémoire du projet

Projet de Matteo (GitHub Lucadevzz) : une monnaie privée indexée sur l'or, **1 Menger (MNGR) = 3,545 g d'or fin**. L'objectif est la version la plus décentralisée possible, celle qui aurait plu à Menger, Mises et Hayek. Matteo n'est pas développeur : il faut lui expliquer simplement, en français, avec des réponses courtes.

## Deux versions dans ce dépôt

- **Racine : v0 sur Ethereum.** Fork de Liquity V1 (Solidity 0.6.11, Foundry). L'unité de dette est le Menger, la garantie est l'ETH. Il n'y a pas de jeton LQTY : les frais vont au pool de stabilité. Les changements par rapport à Liquity sont marqués `MENGER:` et le diff est dans `audit/liquity-v1-to-menger.diff`. Le rapport pour les auditeurs est `audit/AUDIT.md`, le guide de déploiement `DEPLOIEMENT.md`.
  - **Démo en ligne** : https://menger.money (GitHub Pages depuis `main`, dossier `/`, fichier `CNAME`). Le domaine est chez Cloudflare, en DNS seulement (nuage gris).
  - **Sepolia**, déployé le 4 novembre 2026 : les adresses sont dans `deployments/11155111.json`. Le déployeur `0xe0cE…9f69` est le compte de Matteo et contrôle les 4 sources de prix de test. Les rachats sont bloqués jusqu'au 18 novembre 2026 (période de démarrage de Liquity).
  - Pour les scripts de démo sur Sepolia, utiliser `--gas-estimate-multiplier 500`. Le déploiement, lui, se fait avec 200.
- **`lightning/` : version Bitcoin.** C'est la direction choisie le 4 novembre 2026. Des canaux stables en or sur Lightning (idée des Stable Channels, GPL v3), en Rust : `core` contient les règles pures, `node` le nœud (LDK Node 0.8 en version de développement, voir Décisions), le programme `menger-ln` et les tests de bout en bout `node/tests/regtest.rs`. Voir `lightning/README.md`, et pour les relecteurs `lightning/PROTOCOL.md` (règles et messages) et `lightning/SECURITY.md` (ce que chacun peut perdre).
  - L'application de l'épargnant est une page web servie par son nœud sur 127.0.0.1:8421 (`node/src/interface.html`, `web.rs`, `api.rs`). Protections : écoute locale seulement, contrôle de l'en-tête `Host`, secret par lancement dans l'en-tête `x-menger-jeton`, page interdite dans un cadre, texte venu du réseau toujours inséré en texte.
  - `menger-ln essai --bitcoind CHEMIN` : réseau privé avec fournisseur (8420), Alice (8421) et Bruno (8422), prix fictifs modifiables depuis la page, un bloc toutes les 10 secondes.
  - Clés : phrase de récupération de 12 mots (BIP 39) dans `phrase-de-recuperation.txt` du dossier de données (droits 600) ; les nœuds créés avant gardent `ldk/keys_seed` ; `--restaurer` lit la phrase au démarrage.
  - Un fournisseur sert plusieurs épargnants. Un épargnant n'écoute pas le réseau : c'est lui qui appelle son fournisseur.
  - Un épargnant paie une facture Lightning depuis son solde (la position est réduite d'abord) et ce qu'il reçoit est stabilisé automatiquement. Le paiement passe par le fournisseur.
  - Depuis le 11 novembre 2026 : reprendre en bitcoin, annuler un paiement, quitter son fournisseur, retirer vers une adresse, revenir ; canal en retour ouvert par le fournisseur (`--canal-retour`) ; fichier de sources de prix (`--prices sources:FICHIER`).

## Décisions

- **Unité or plutôt que dollar.** Un Menger en dollars ferait doublon avec LUSD et ses copies.
- **Nom.** Ducat, puis Catallax, puis **Menger** (choisi le 3 novembre 2026). Ducat Protocol occupe « Ducat », et un protocole Nostr s'appelle déjà Catallax.
- **Bitcoin plutôt qu'Ethereum** pour la version finale. Les canaux Lightning ne demandent ni jeton, ni pont, ni oracle commun : chaque partie lit ses propres prix. Les covenants ne sont pas activés sur Bitcoin. Citrea a été écarté à cause de son conseil de sécurité, et le WBTC parce qu'il dépend d'un dépositaire.
- **Le fournisseur fixe le prix d'un changement de position** (messages version 3, 11 novembre 2026). L'épargnant donne les sats et la pire quantité de Mengers qu'il accepte (0,3 % d'écart par défaut) ; le fournisseur calcule avec son propre prix, arrondi en sa faveur. Avant, l'épargnant nommait les Mengers et une demande « reprendre des sats sans rendre de Mengers » passait : un épargnant malhonnête pouvait vider son fournisseur. Chaque message nomme son destinataire, signé avec le reste.
- **Une absence n'est pas une faute.** Une partie injoignable a un délai plus long pour payer : 6 h quand c'est l'épargnant qui attend, 72 h quand c'est le fournisseur. Avant, l'épargnant prenait un fournisseur injoignable pour un fournisseur insolvable et fermait de force aussitôt.
- **Le fournisseur vérifie que l'épargnant détient toujours les sats de sa position.** Sinon il ne paie plus, puis ferme : un épargnant pourrait sortir ses sats du canal et garder la protection contre la baisse.
- **Les soldes se lisent dans les canaux, sans les frais de fermeture** (ce qu'on peut envoyer plus la réserve du canal). Le montant « récupérable à la fermeture » bouge avec les frais du réseau et faisait apparaître de faux écarts.
- **Un paiement est noté avant d'être envoyé**, sous une identité choisie d'avance ; au redémarrage, le programme demande au nœud ce qu'il est devenu.
- **Prix de l'or** : trois familles qui doivent concorder à 1 % près, deux suffisent : le comptant (Swissquote), le PAXG (Kraken, Coinbase, Bitstamp) et le XAUT (Kraken). Les sources d'une même famille comptent pour une. Kraken et Bitstamp : milieu de l'achat et de la vente. Le prix de l'or est gelé du vendredi 21 h au lundi 0 h UTC.
- **Frais d'un paiement** : jamais plus de 1 % du montant plus 50 sats, mis de côté avant l'envoi ; le reste retourne dans la position.
- **Le fournisseur se déclare service LSPS2** : avec LDK Node, c'est ce qui lui permet de transmettre des paiements vers les canaux privés des épargnants. Un secret tiré au hasard à chaque lancement est exigé pour ses canaux à la demande : personne ne le reçoit, ils ne servent pas. Les épargnants ne sont **pas** clients LSPS2 : ils n'acceptent jamais un canal non confirmé (pas de confiance).
- **LDK Node 0.8 en version de développement** (dépôt git, commit `92d703b`, 10 novembre 2026), choisi par Matteo le 5 novembre 2026 : avec LDK 0.3, un canal privé accepte des paiements aussi gros que lui (avec LDK Node 0.7, c'était 10 % du canal ouvert par l'autre partie). LDK Node repose sur la troisième version candidate de LDK 0.3. La quatrième (11 novembre 2026) corrige d'autres défauts, mais son client Esplora ne compile pas (`outpoint.index` au lieu de `outpoint.vout` dans `lightning-transaction-sync/src/esplora.rs`) : attendre que LDK Node la reprenne. Passer à la 0.8 officielle dès sa sortie, et avant tout vrai bitcoin.
- **Délai de punition** : 144 blocs par défaut, donc l'épargnant doit se connecter au moins une fois par jour. LDK permet deux semaines, LDK Node ne l'expose pas : contribution à proposer aux auteurs de LDK Node (à faire au nom de Matteo seulement avec son accord).
- **Stabilisation automatique** de ce qu'un épargnant reçoit : elle attend qu'aucun paiement ne soit en cours dans ses canaux (sinon le solde n'est pas encore à jour) ; si le fournisseur refuse, les sats sont reproposés 5 minutes plus tard.
- **Une ouverture de canal à la fois** par portefeuille : LDK Node ne réserve pas les pièces d'une transaction d'ouverture, et deux ouvertures rapprochées ont dépensé la même pièce (vu le 5 novembre 2026). `open_channel` refuse tant que la précédente n'est pas connue du portefeuille ou confirmée.
- **Licence GPL v3** partout.

## Commandes

```bash
forge test                                               # v0 : 71 tests
cd lightning && cargo test --workspace                   # règles, sources, clés, application (45 + 18 tests)
BITCOIND_EXE=/chemin/bitcoind cargo test --workspace     # + les 4 tests de bout en bout (regtest)
cargo run --release -- essai --bitcoind /chemin/bitcoind # l'application dans le navigateur, prix fictifs
cargo run --release -- demo --bitcoind /chemin/bitcoind  # démonstration commentée
cargo run --release --example prix                       # prix réels
cargo run --release -- sources                           # la liste des sources de prix, à modifier
```

`.github/workflows/tests.yml` lance tout cela sur GitHub à chaque changement.

Dans le bac à sable de Claude :

- `bitcoind` n'est pas installé : le compiler depuis les sources (v30.4, sans portefeuille ; `apt-get update` puis `libevent-dev libboost-dev`).
- `forge` : `npm install @foundry-rs/forge` (le binaire est dans `node_modules/@foundry-rs/forge-linux-amd64/bin`). Le compilateur Solidity 0.6.11 se télécharge depuis les « releases » GitHub d'`ethereum/solidity` (`solc-static-linux`), à placer dans `~/.svm/0.6.11/solc-0.6.11` ; puis `forge test --offline`. `binaries.soliditylang.org` est bloqué.
- Les requêtes HTTP passent par le proxy défini dans `HTTPS_PROXY`. `bitcoincore.org` et `static.rust-lang.org` sont bloqués ; `git clone` de dépôts publics et `raw.githubusercontent.com` passent.
- Le disque est limité : `cargo clean` si la place manque (les dépendances sont compilées sans informations de débogage pour rester petites).
- L'API GitHub des « Actions » n'est pas accessible : demander à Matteo si la coche est verte.

## Prochaines étapes

1. Faire tourner `menger-ln` sur Mutinynet (`cargo run --example mutinynet`). Le robinet refuse le compte GitHub de Matteo, mais accepte un paiement Lightning de 50 sats à la place (`GET https://faucet.mutinynet.com/api/l402`, puis `Authorization: L402 jeton:preimage`) : il faut que Matteo paie la facture, valable 10 minutes, et donne la preuve de paiement.
2. Une relecture par un développeur Bitcoin et Lightning (lui donner `PROTOCOL.md` et `SECURITY.md`), puis un audit avant tout usage de vrais BTC.
3. Une application mobile pour l'épargnant, et des programmes tout prêts à télécharger.
4. Faire tester la démo Ethereum à 20 ou 50 personnes et demander aux bitcoiners s'ils emprunteraient de l'or contre leurs BTC.

## Règles de travail

- Le texte destiné à Matteo et aux utilisateurs est en français, avec les espaces typographiques. Le code et les commentaires sont en anglais.
- Tous les tests doivent passer avant de pousser sur `main`.
- Ne jamais mettre de clé privée ni de secret dans le dépôt.
