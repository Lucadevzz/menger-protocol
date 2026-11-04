# Menger sur Lightning, prototype

Un solde qui garde sa valeur en or, **1 Menger = 3,545 g d'or fin**, réglé en bitcoin dans un canal Lightning. Chacun garde ses clés et ses propres bitcoins. Il n'y a ni jeton, ni pont, ni émetteur, ni administrateur, ni oracle commun.

> **Prototype de recherche, non audité.** Testé de bout en bout sur un réseau Bitcoin privé, avec deux vrais nœuds Lightning. Il n'a pas encore tourné sur un réseau public. N'y mets jamais de vrais bitcoins avant une relecture par un développeur Bitcoin.

## Comment ça marche

1. **Deux parties.** L'*épargnant* veut un solde stable en or. Le *fournisseur* prend l'autre côté : il gagne quand le bitcoin monte plus vite que l'or et perd quand il baisse. Chacun ouvre un canal vers l'autre avec ses propres bitcoins.
2. **Une position.** L'épargnant propose de garder, par exemple, 1 000 000 sats stables. Au prix du moment, cela fait 1,7 Menger, soit 6 g d'or. Le fournisseur accepte si :
   - le prix annoncé est à moins de 1 % du sien ;
   - le prix de la stabilité lui convient ;
   - il a assez de bitcoins pour couvrir une baisse.
3. **Des paiements dans les deux sens.** Toutes les 30 secondes, chacun calcule ce que vaut la promesse en or, avec **ses propres sources de prix**. Si le bitcoin de l'épargnant vaut plus que sa promesse, l'épargnant paie l'excédent au fournisseur. S'il vaut moins, le fournisseur complète. Les écarts de moins de 0,1 % sont ignorés.
4. **La seule sanction, c'est de partir.** Celui à qui l'autre doit plus de 2 % depuis plus de 10 minutes ferme le canal et repart avec ce qu'il détient. L'épargnant ferme aussi si le fournisseur ne peut plus couvrir une baisse de 10 %.
5. **Le week-end.** L'or ne cote pas du vendredi 21 h au lundi 0 h (UTC). On garde alors le dernier prix de l'or du vendredi, tandis que le bitcoin continue de bouger.
6. **Pas de prix fiable, pas de paiement.** Si les sources sont périmées ou ne concordent pas, personne ne paie et personne ne ferme.

Les messages entre les deux parties voyagent dans les paiements Lightning eux-mêmes : il n'y a pas de serveur.

## Pourquoi ce design

- **Menger** : l'unité est un poids d'or, la monnaie que les échanges ont choisie d'eux-mêmes. Le règlement se fait en bitcoin, la monnaie qui naît aujourd'hui de la même façon.
- **Mises** : la réserve est pleine. Chaque Menger est couvert par de vrais bitcoins bloqués dans le canal, et rien n'est créé à partir de rien : ni crédit, ni jeton.
- **Hayek** : il n'y a ni émetteur ni oracle central. Chacun choisit ses sources de prix. Le prix de la stabilité se négocie entre l'épargnant et son fournisseur, et n'importe qui peut devenir fournisseur : les fournisseurs se font concurrence.

L'idée de canaux stables vient des [Stable Channels](https://github.com/toneloc/stable-channels) de Tony Klausing, qui le font en dollars. Ce code est écrit à part, en plus simple, et compte en or.

## Ce qui a été testé

| Test | Résultat |
|---|---|
| Règles (unité, prix, stabilité, messages) | 28 tests réussis, dont deux sur des milliers de cas aléatoires |
| Sources de prix, état sauvegardé | 9 tests réussis |
| Bout en bout, réseau Bitcoin privé, deux nœuds Lightning | Le bitcoin baisse, remonte, l'or monte, week-end, krach à 70 000 $ : les deux registres concordent et l'épargnant garde ses 2,031467 MNGR. Fournisseur muet : l'épargnant ferme et récupère ses bitcoins sur la chaîne. |
| Prix réels, 4 novembre 2026 | BTC 78 520 $ (Kraken, Bitstamp, Coinbase), or 4 040 $ l'once (Swissquote, PAXG) : 1 Menger = 460,40 $ = 586 340 sats |

## Essayer

Il faut [Rust](https://rustup.rs) et Bitcoin Core (sur Mac : `brew install bitcoin`).

```bash
cd lightning
cargo run --release -- demo --bitcoind "$(which bitcoind)"   # démonstration sur un réseau privé
cargo run --release --example prix                            # prix réels du Menger
BITCOIND_EXE="$(which bitcoind)" cargo test --workspace       # tous les tests
```

Sur un réseau de test public, ce n'est pas encore essayé. Avec [Mutinynet](https://faucet.mutinynet.com) (des blocs toutes les 30 secondes et un robinet gratuit), chaque partie lance son propre nœud :

```bash
# Le fournisseur
cargo run --release -- run --role fournisseur --data ~/menger-fournisseur \
  --network signet --esplora https://mutinynet.com/api --listen 0.0.0.0:9735
# L'épargnant : ouvre un canal de 200 000 sats et en stabilise 100 000
cargo run --release -- run --role epargnant --data ~/menger-epargnant \
  --network signet --esplora https://mutinynet.com/api --listen 0.0.0.0:9736 \
  --open CLE_DU_FOURNISSEUR@IP:9735:200000 --stabilize 100000
```

Le fournisseur ouvre aussi un canal vers l'épargnant (`--open`), pour avoir de quoi le payer. Le dossier `--data` contient les clés : c'est lui qu'il faut sauvegarder. `--rate-ppm 20000` fixe le prix de la stabilité à 2 % par an, payé par l'épargnant ; un nombre négatif le fait payer par le fournisseur.

## Limites connues

1. **La stabilité tient jusqu'au dernier paiement, pas au-delà.** Si le fournisseur cesse de payer pendant un krach, l'épargnant ferme le canal. Il récupère alors ses bitcoins au cours du moment, mais perd la baisse survenue depuis le dernier paiement, dans la limite du délai de grâce (10 minutes). Une fermeture forcée, quand l'autre partie est hors ligne, bloque en plus les fonds quelques jours, et pendant ce temps ils ne sont plus stabilisés.
2. **Le fournisseur doit avoir assez de bitcoins.** Avec autant de bitcoins que l'épargnant, il couvre une baisse de 50 % du bitcoin face à l'or.
3. **Le Menger est un solde, pas une pièce.** On ne peut pas encore payer un tiers en Mengers : il faudrait faire passer les paiements par le fournisseur, ce que Stable Channels prévoit.
4. **Une seule position par paire de parties**, et pas encore d'interface graphique : c'est un programme en ligne de commande.
5. **Les sources de l'or** : une source au comptant (Swissquote) et le prix du jeton PAXG sur Kraken, qui doivent concorder à 1 % près. Seul le prix du PAXG est lu, rien n'est détenu. goldprice.org refuse les requêtes automatiques. Une deuxième source au comptant serait préférable.
6. **Les gros mouvements sont payés en plusieurs fois.** Lightning limite chaque paiement à 10 % de la capacité du canal.
7. Les prix peuvent être lus à travers Tor : `ALL_PROXY=socks5://127.0.0.1:9050`.

## Fichiers

```
core/    les règles, sans réseau ni disque : unité, prix, stabilité, messages
node/    le nœud Lightning (LDK Node), les sources de prix, la sauvegarde, le programme menger-ln
node/tests/regtest.rs   le test de bout en bout sur un réseau Bitcoin privé
```

## Licence

GPL v3, comme le reste du projet. Le code utilise [LDK Node](https://github.com/lightningdevkit/ldk-node) (MIT ou Apache 2.0).
