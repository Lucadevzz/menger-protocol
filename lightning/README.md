# Menger sur Lightning, prototype

Un solde qui garde sa valeur en or, **1 Menger = 3,545 g d'or fin**, réglé en bitcoin dans des canaux Lightning. On peut le garder, le payer à quelqu'un et en recevoir, depuis une application simple dans le navigateur. Chacun garde ses clés et ses propres bitcoins. Il n'y a ni jeton, ni pont, ni émetteur, ni administrateur, ni oracle commun.

> **Prototype de recherche, non audité.** Testé de bout en bout sur un réseau Bitcoin privé, avec de vrais nœuds Lightning. Il n'a pas encore tourné sur un réseau public. N'y mets jamais de vrais bitcoins avant une relecture par un développeur Bitcoin.

## Comment ça marche

1. **Deux rôles.** L'*épargnant* veut un solde stable en or. Le *fournisseur* prend l'autre côté : il gagne quand le bitcoin monte plus vite que l'or et perd quand il baisse. Un fournisseur sert autant d'épargnants qu'il veut. L'épargnant ouvre un canal vers son fournisseur avec ses propres bitcoins, et le fournisseur en ouvre un vers lui pour avoir de quoi le payer.
2. **Une position.** L'épargnant demande à garder, par exemple, 1 000 000 sats stables : au prix de test, 2,03 Menger, soit 7,2 g d'or. Le fournisseur accepte si :
   - le prix annoncé est à moins de 1 % du sien ;
   - ces sats sont bien dans les canaux de l'épargnant ;
   - il a assez de bitcoins pour couvrir une baisse de 30 %.

   La position peut ensuite grandir ou diminuer, toujours au prix du moment et avec l'accord du fournisseur.
3. **Des paiements dans les deux sens.** Toutes les 30 secondes, chacun calcule ce que vaut la promesse en or, avec **ses propres sources de prix**. Si le bitcoin de l'épargnant vaut plus que sa promesse, l'épargnant paie l'excédent au fournisseur. S'il vaut moins, le fournisseur complète. Les écarts de moins de 0,1 % sont ignorés, et celui qui paie ramène l'écart à 0,05 % : deux parties dont les prix diffèrent un peu ne se renvoient pas la différence.
4. **Payer et recevoir en Mengers.** Bruno crée une facture Lightning de 0,3 MNGR, en sats au prix du moment. Alice la paie depuis son solde : avec ses sats libres d'abord, sinon son fournisseur réduit d'abord sa position de ce qui manque. Le paiement part ensuite et passe par lui jusqu'à Bruno. Dès qu'il est arrivé, le nœud de Bruno demande à ajouter ces sats à sa position : Bruno garde 0,3 MNGR, même si le bitcoin bouge ensuite. C'est un paiement Lightning ordinaire, qui arrive en entier ou pas du tout. N'importe quel portefeuille Lightning peut donc payer un épargnant, ou être payé par lui.
5. **La seule sanction, c'est de partir.** Celui à qui l'autre doit plus de 2 % depuis plus de 10 minutes ferme ses canaux et repart avec ce qu'il détient. L'épargnant ferme aussi si le fournisseur ne peut plus couvrir une baisse de 10 %.
6. **Le week-end.** L'or ne cote pas du vendredi 21 h au lundi 0 h (UTC). On garde alors le dernier prix de l'or du vendredi, tandis que le bitcoin continue de bouger.
7. **Pas de prix fiable, pas de paiement.** Si les sources sont périmées ou ne concordent pas, personne ne paie et personne ne ferme.

Les messages entre les parties voyagent dans les paiements Lightning eux-mêmes : il n'y a pas de serveur. Chaque message est signé avec la clé du nœud qui l'envoie, si bien que personne ne peut se faire passer pour un autre. Une demande sans réponse est renvoyée, et une demande déjà traitée reçoit la même réponse : les registres des deux côtés ne peuvent pas diverger.

## Pourquoi ce design

- **Menger** : l'unité est un poids d'or, la monnaie que les échanges ont choisie d'eux-mêmes. Le règlement se fait en bitcoin, la monnaie qui naît aujourd'hui de la même façon.
- **Mises** : la réserve est pleine. Chaque Menger est couvert par de vrais bitcoins bloqués dans les canaux, et rien n'est créé à partir de rien : ni crédit, ni jeton.
- **Hayek** : il n'y a ni émetteur ni oracle central. Chacun choisit ses sources de prix. Le prix de la stabilité se négocie entre l'épargnant et son fournisseur, et n'importe qui peut devenir fournisseur : les fournisseurs se font concurrence.

L'idée de canaux stables vient des [Stable Channels](https://github.com/toneloc/stable-channels) de Tony Klausing, qui le font en dollars. Ce code est écrit à part, en plus simple, et compte en or.

## Ce qui a été testé

| Test | Résultat |
|---|---|
| Règles (unité, prix, stabilité, changements de position, messages) | 34 tests réussis, dont deux sur des milliers de cas aléatoires |
| Sources de prix, état sauvegardé, phrase de récupération, application | 13 tests réussis, dont un qui vérifie qu'une page sans le secret est refusée |
| Bout en bout, un épargnant et son fournisseur | Le bitcoin baisse, remonte, l'or monte, week-end : les deux registres concordent et l'épargnant garde ses 2,031467 MNGR. Fournisseur muet : l'épargnant ferme et récupère ses bitcoins sur la chaîne. |
| Bout en bout, deux épargnants et un fournisseur | Alice paie 0,5 MNGR à Bruno, par le fournisseur, d'un seul paiement (plus de 10 % du canal de Bruno). Bruno, qui n'a jamais mis un bitcoin dans un canal, garde ses 0,5 MNGR quand le bitcoin baisse de 10 %. Les prix du fournisseur s'écartent ensuite de 0,12 % de ceux des épargnants : il complète une fois et personne ne renvoie la différence. Bruno rend enfin 0,2 MNGR à Alice. Les registres concordent à chaque étape. |
| Application, dans un navigateur | Sur le réseau privé de l'essai : Bruno crée une facture de 0,3 MNGR, Alice la vérifie et la paie, puis une baisse de 20 % est compensée d'un seul paiement. |
| Phrase de récupération | Un portefeuille restauré depuis ses 12 mots retrouve la même clé de nœud et la même adresse. |
| Prix réels, 4 novembre 2026 | BTC 78 520 $ (Kraken, Bitstamp, Coinbase), or 4 040 $ l'once (Swissquote, PAXG) : 1 Menger = 460,40 $ = 586 340 sats |

## Essayer

Il faut [Rust](https://rustup.rs) et Bitcoin Core (sur Mac : `brew install bitcoin`).

```bash
cd lightning
cargo run --release -- essai --bitcoind "$(which bitcoind)"  # l'application, dans le navigateur, avec des prix fictifs
cargo run --release -- demo --bitcoind "$(which bitcoind)"   # démonstration commentée dans le terminal
cargo run --release --example prix                            # prix réels du Menger
BITCOIND_EXE="$(which bitcoind)" cargo test --workspace       # tous les tests
```

**L'essai** lance un réseau Bitcoin privé sur ton ordinateur, avec un fournisseur et deux épargnants, Alice et Bruno. Chacun a sa page : http://127.0.0.1:8421 pour Alice, 8422 pour Bruno, 8420 pour le fournisseur. Alice garde déjà 0,015 BTC en or. Des boutons font monter ou baisser le prix du bitcoin et de l'or, et on voit les soldes en or tenir. Bruno peut créer une facture en Mengers, et Alice la payer.

**L'application** de l'épargnant est cette même page, servie par son nœud et visible seulement sur son ordinateur. Elle montre le solde en Mengers et en grammes d'or, et permet de recevoir (facture et QR code), de payer, d'épargner en or et de déposer des bitcoins.

Sur un réseau de test public, ce n'est pas encore essayé. Avec [Mutinynet](https://faucet.mutinynet.com) (des blocs toutes les 30 secondes et un robinet gratuit), chaque partie lance son propre nœud :

```bash
# Le fournisseur
cargo run --release -- run --role fournisseur --data ~/menger-fournisseur \
  --network signet --esplora https://mutinynet.com/api --listen 0.0.0.0:9735
# L'épargnant : ouvre un canal de 400 000 sats vers son fournisseur
cargo run --release -- run --role epargnant --data ~/menger-epargnant \
  --network signet --esplora https://mutinynet.com/api --listen 0.0.0.0:9736 \
  --fournisseur CLE_DU_FOURNISSEUR@IP:9735 --canal 400000
```

L'application s'ouvre alors sur http://127.0.0.1:8421 (`--interface` pour changer l'adresse, `--interface aucune` pour s'en passer). Le programme lit aussi des commandes dans le terminal : `etat`, `stabiliser 200000` (ou `stabiliser tout`), `facture 0,3`, `payer FACTURE`, `canal CLÉ@IP:PORT SATS`, `adresse`, `aide`. Le fournisseur ouvre un canal vers chaque épargnant avec `canal`, pour avoir de quoi le payer.

Au premier lancement, le programme affiche une **phrase de récupération de 12 mots**, à écrire sur papier. Avec `--restaurer`, dans un dossier neuf, elle redonne accès aux bitcoins du portefeuille. Pour l'argent des canaux, il faut en plus sauvegarder le dossier `--data`, qui garde l'état de chaque canal. `--rate-ppm 20000` fixe le prix de la stabilité à 2 % par an, payé par l'épargnant ; un nombre négatif le fait payer par le fournisseur. `--sans-stabilisation-auto` garde en bitcoin ce que l'épargnant reçoit.

## Limites connues

1. **La stabilité tient jusqu'au dernier paiement, pas au-delà.** Si le fournisseur cesse de payer pendant un krach, l'épargnant ferme ses canaux. Il récupère alors ses bitcoins au cours du moment, mais perd la baisse survenue depuis le dernier paiement, dans la limite du délai de grâce (10 minutes). Une fermeture forcée, quand l'autre partie est hors ligne, bloque en plus les fonds quelques jours, et pendant ce temps ils ne sont plus stabilisés.
2. **Le fournisseur doit avoir assez de bitcoins.** Avec autant de bitcoins que l'épargnant, il couvre une baisse de 50 % du bitcoin face à l'or.
3. **Une version de développement de LDK Node.** Le projet utilise LDK Node 0.8 dans son état du 29 octobre 2026, avant sa sortie officielle : elle repose sur LDK 0.3, où un canal privé accepte des paiements aussi gros que lui, sans que l'épargnant ait à utiliser un canal non confirmé. Il faudra passer à la version officielle dès sa sortie, et en tout cas avant de vrais bitcoins.
4. **Se connecter au moins une fois par jour.** Si le fournisseur publiait un ancien état d'un canal, le nœud de l'épargnant aurait 144 blocs, environ un jour, pour le punir. LDK permet d'allonger ce délai à deux semaines, mais LDK Node ne l'expose pas encore : c'est une petite contribution à proposer à ses auteurs. Aucun service de garde (« watchtower ») ne fonctionne encore avec LDK Node.
5. **Le fournisseur voit passer les paiements de ses épargnants**, comme tout nœud Lightning qui transmet un paiement. Il peut refuser d'en transmettre un, qui échoue alors sans rien coûter, mais il ne peut pas le détourner.
6. **Une ouverture de canal à la fois par portefeuille.** LDK Node ne met pas de côté les pièces d'une transaction d'ouverture qu'il signe : deux ouvertures rapprochées peuvent dépenser la même pièce, et la seconde ne serait jamais confirmée. Le programme attend donc que la précédente soit connue du portefeuille ; avec un nœud Bitcoin Core, qu'elle soit confirmée.
7. **Le fournisseur se déclare service de liquidité LSPS2** (bLIP-52). Avec LDK Node, c'est ce qui lui permet de transmettre des paiements vers les canaux privés de ses épargnants. Les canaux à la demande de LSPS2 ne servent pas encore.
8. **L'application se lance depuis un terminal**, et toute personne qui utilise le même ordinateur peut l'ouvrir. Une application mobile, qui garderait les clés sur le téléphone, reste à faire.
9. **Les sources de l'or** : une source au comptant (Swissquote) et le prix du jeton PAXG sur Kraken, qui doivent concorder à 1 % près. Seul le prix du PAXG est lu, rien n'est détenu. goldprice.org refuse les requêtes automatiques. Une deuxième source au comptant serait préférable.
10. Les prix peuvent être lus à travers Tor : `ALL_PROXY=socks5://127.0.0.1:9050`.

## Fichiers

```
core/    les règles, sans réseau ni disque : unité, prix, stabilité, changements de position, messages
node/    le nœud Lightning (LDK Node), les sources de prix, la sauvegarde, le programme menger-ln
node/src/interface.html    l'application, une seule page ; web.rs la sert, api.rs fait le travail de ses boutons
node/src/keys.rs           la phrase de récupération
node/tests/regtest.rs      les tests de bout en bout sur un réseau Bitcoin privé
node/examples/mutinynet.rs un épargnant et un fournisseur sur Mutinynet, avec les vrais prix
```

## Licence

GPL v3, comme le reste du projet. Le code utilise [LDK Node](https://github.com/lightningdevkit/ldk-node) (MIT ou Apache 2.0).
