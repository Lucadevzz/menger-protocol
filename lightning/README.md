# Menger sur Lightning, prototype

Un solde qui garde sa valeur en or, **1 Menger = 3,545 g d'or fin**, réglé en bitcoin dans des canaux Lightning. On peut le garder, le payer à quelqu'un et en recevoir, depuis une application simple dans le navigateur. Chacun garde ses clés et ses propres bitcoins. Il n'y a ni jeton, ni pont, ni émetteur, ni administrateur, ni oracle commun.

> **Prototype de recherche, non audité.** Testé de bout en bout sur un réseau Bitcoin privé, avec de vrais nœuds Lightning. Il n'a pas encore tourné sur un réseau public. N'y mets jamais de vrais bitcoins avant une relecture par un développeur Bitcoin : le programme refuse d'ailleurs le vrai réseau, sauf si on le force.

## Comment ça marche

1. **Deux rôles.** L'*épargnant* veut un solde stable en or. Le *fournisseur* prend l'autre côté : il gagne quand le bitcoin monte plus vite que l'or et perd quand il baisse. Un fournisseur sert autant d'épargnants qu'il veut. L'épargnant ouvre un canal vers son fournisseur avec ses propres bitcoins, et le fournisseur en ouvre un vers lui pour avoir de quoi le payer.
2. **Une position.** L'épargnant demande à garder, par exemple, 1 000 000 sats stables. Il donne les sats et la plus petite quantité de Mengers qu'il accepte en échange (0,3 % sous son propre prix, par défaut). **Le fournisseur calcule avec son propre prix** et répond par la quantité exacte : au prix de test, 2,03 Menger, soit 7,2 g d'or. Il accepte si :
   - son prix reste dans la limite donnée par l'épargnant ;
   - ces sats sont bien dans les canaux de l'épargnant ;
   - il a assez de bitcoins pour couvrir une baisse de 30 %.

   La position peut ensuite grandir ou diminuer de la même façon. Aucun des deux n'a à croire le prix de l'autre : le fournisseur ne donne jamais plus que ce que valent les sats à son prix, et l'épargnant ne reçoit jamais moins que sa limite.
3. **Des paiements dans les deux sens.** Toutes les 30 secondes, chacun calcule ce que vaut la promesse en or, avec **ses propres sources de prix**. Si le bitcoin de l'épargnant vaut plus que sa promesse, l'épargnant paie l'excédent au fournisseur. S'il vaut moins, le fournisseur complète. Les écarts de moins de 0,1 % sont ignorés, et celui qui paie ramène l'écart à 0,05 % : deux parties dont les prix diffèrent un peu ne se renvoient pas la différence.
4. **Payer et recevoir en Mengers.** Bruno crée une facture Lightning de 0,3 MNGR, en sats au prix du moment. Alice la paie depuis son solde : avec ses sats libres d'abord, sinon son fournisseur réduit d'abord sa position de ce qui manque. Le paiement part ensuite et passe par lui jusqu'à Bruno ; ses frais ne dépassent jamais 1 % du montant plus 50 sats. Dès qu'il est arrivé, le nœud de Bruno demande à ajouter ces sats à sa position : Bruno garde 0,3 MNGR, même si le bitcoin bouge ensuite. C'est un paiement Lightning ordinaire, qui arrive en entier ou pas du tout. N'importe quel portefeuille Lightning relié au fournisseur peut donc payer un épargnant, ou être payé par lui.
5. **La seule sanction, c'est de partir.** Celui à qui l'autre doit plus de 2 % depuis plus de 10 minutes ferme ses canaux et repart avec ce qu'il détient. L'épargnant ferme aussi si le fournisseur ne peut plus couvrir une baisse de 10 %. Le fournisseur ne paie plus, puis ferme, si l'épargnant a sorti de ses canaux les sats que compte sa position.
6. **Une absence n'est pas une faute.** Celui qui est injoignable ne peut pas payer : il a plus de temps. Un épargnant attend son fournisseur 6 heures avant de sortir de force ; un fournisseur attend un épargnant 72 heures (son ordinateur dort la nuit et le week-end). De retour, chacun a de nouveau 10 minutes. Chaque partie règle ces délais pour elle-même. Et quand c'est notre propre ordinateur qui a dormi, les délais repartent de zéro au réveil : on ne ferme pas un canal sur une absence qu'on n'a pas pu voir.
7. **Partir quand on veut.** L'épargnant peut reprendre son solde en bitcoin (s'il doit quelques sats au fournisseur, son nœud les paie d'abord, tout seul), quitter son fournisseur (les canaux se ferment et les bitcoins reviennent sur la chaîne), les retirer vers n'importe quelle adresse, puis revenir, ou aller chez un autre fournisseur.
8. **Le week-end.** L'or ne cote pas du vendredi 21 h au lundi 0 h (UTC). On garde alors le dernier prix de l'or du vendredi, tandis que le bitcoin continue de bouger.
9. **Pas de prix fiable, pas de paiement.** Si les sources sont périmées ou ne concordent pas, personne ne paie et personne ne ferme.

Les messages entre les parties voyagent dans les paiements Lightning eux-mêmes : il n'y a pas de serveur. Chaque message nomme son destinataire et est signé avec la clé du nœud qui l'envoie : personne ne peut se faire passer pour un autre, ni faire suivre à quelqu'un un message qui ne lui était pas destiné. Une demande sans réponse est renvoyée, et une demande déjà traitée reçoit la même réponse : les registres des deux côtés ne peuvent pas diverger. Chaque paiement est noté avant d'être envoyé : après une coupure de courant, le programme retrouve ce qu'il est devenu.

Les règles exactes sont dans [PROTOCOL.md](PROTOCOL.md) et ce que chaque partie peut perdre dans [SECURITY.md](SECURITY.md) (en anglais, pour les relecteurs).

## Pourquoi ce design

- **Menger** : l'unité est un poids d'or, la monnaie que les échanges ont choisie d'eux-mêmes. Le règlement se fait en bitcoin, la monnaie qui naît aujourd'hui de la même façon.
- **Mises** : la réserve est pleine. Chaque Menger est couvert par de vrais bitcoins bloqués dans les canaux, et rien n'est créé à partir de rien : ni crédit, ni jeton.
- **Hayek** : il n'y a ni émetteur ni oracle central. Chacun choisit ses sources de prix, dans un fichier qu'il peut modifier. Le prix de la stabilité et la marge du fournisseur se négocient entre l'épargnant et son fournisseur, et n'importe qui peut devenir fournisseur : les fournisseurs se font concurrence.

L'idée de canaux stables vient des [Stable Channels](https://github.com/toneloc/stable-channels) de Tony Klausing, qui le font en dollars. Ce code est écrit à part, en plus simple, et compte en or.

## Ce qui a été testé

| Test | Résultat |
|---|---|
| Règles (unité, prix, stabilité, changements de position, messages) | 46 tests réussis, dont cinq sur des centaines ou des milliers de cas aléatoires : aucun changement accepté ne coûte au fournisseur à son propre prix, et aucune entrée, même absurde, ne fait planter les règles |
| Sources de prix, état sauvegardé, phrase de récupération, application | 18 tests réussis, dont un qui vérifie qu'une page sans le secret est refusée |
| Bout en bout, un épargnant et son fournisseur | Le bitcoin baisse, remonte, l'or monte, week-end : les deux registres concordent et l'épargnant garde ses 2,031467 MNGR. Fournisseur muet : l'épargnant ferme et récupère ses bitcoins sur la chaîne. |
| Bout en bout, deux épargnants et un fournisseur | Alice paie 0,5 MNGR à Bruno, par le fournisseur, d'un seul paiement (plus de 10 % du canal de Bruno). Bruno, qui n'a jamais mis un bitcoin dans un canal, garde ses 0,5 MNGR quand le bitcoin baisse de 10 %. Les prix du fournisseur s'écartent ensuite de 0,12 % de ceux des épargnants : il complète une fois et personne ne renvoie la différence. Bruno rend enfin 0,2 MNGR à Alice. Les registres concordent à chaque étape. |
| Bout en bout, absences et départs | Le fournisseur ouvre de lui-même un canal en retour. Une demande à un prix trop éloigné du sien est refusée. Il s'absente une heure pendant une baisse de 10 % : l'épargnant ne ferme rien, et il est payé au retour. Les deux nœuds redémarrent sans rien perdre. L'épargnant reprend des sats en bitcoin, quitte son fournisseur, retire ses bitcoins, revient. Le fournisseur disparaît pour de bon : après six heures, l'épargnant sort de force et retrouve ses bitcoins sur la chaîne. |
| Bout en bout, un épargnant malhonnête | Il sort ses sats du canal sans toucher à sa position : le fournisseur ne le paie plus quand le bitcoin baisse, puis ferme. |
| Application, dans un navigateur | Sur le réseau privé de l'essai : Bruno crée une facture, Alice la paie, reprend des sats en bitcoin, quitte son fournisseur, retire vers une adresse, rouvre un canal et garde de nouveau tout en or. |
| Phrase de récupération | Un portefeuille restauré depuis ses 12 mots retrouve la même clé de nœud et la même adresse. |
| Prix réels, 11 novembre 2026 | BTC 74 042 $ (Kraken, Bitstamp, Coinbase), or 3 996 $ l'once (comptant 4 005 $, PAXG 3 996 $, XAUT 3 993 $) : 1 Menger = 455,45 $ = 615 116 sats |

## Essayer

Il faut [Rust](https://rustup.rs) et Bitcoin Core (sur Mac : `brew install bitcoin`).

```bash
cd lightning
cargo run --release -- essai --bitcoind "$(which bitcoind)"  # l'application, dans le navigateur, avec des prix fictifs
cargo run --release -- demo --bitcoind "$(which bitcoind)"   # démonstration commentée dans le terminal
cargo run --release --example prix                            # prix réels du Menger
BITCOIND_EXE="$(which bitcoind)" cargo test --workspace       # tous les tests
```

**L'essai** lance un réseau Bitcoin privé sur ton ordinateur, avec un fournisseur et deux épargnants, Alice et Bruno. Chacun a sa page : http://127.0.0.1:8421 pour Alice, 8422 pour Bruno, 8420 pour le fournisseur. Alice garde déjà 0,015 BTC en or. Des boutons font monter ou baisser le prix du bitcoin et de l'or, et on voit les soldes en or tenir. Bruno peut créer une facture en Mengers, et Alice la payer. On peut aussi quitter le fournisseur, retirer ses bitcoins et rouvrir un canal : un bloc est miné toutes les dix secondes.

**L'application** de l'épargnant est cette même page, servie par son nœud et visible seulement sur son ordinateur. Elle montre le solde en Mengers et en grammes d'or, et permet de recevoir (facture et QR code), de payer, d'épargner en or ou de reprendre en bitcoin, de déposer et de retirer des bitcoins, et de quitter son fournisseur. Elle prévient quand le fournisseur est injoignable, quand il doit quelque chose, ou quand il n'a plus assez pour couvrir une baisse.

Sur un réseau de test public, ce n'est pas encore essayé. Avec [Mutinynet](https://faucet.mutinynet.com) (des blocs toutes les 30 secondes et un robinet gratuit), chaque partie lance son propre nœud :

```bash
# Le fournisseur : il ouvre en retour un canal de la même taille vers chaque épargnant
cargo run --release -- run --role fournisseur --data ~/menger-fournisseur \
  --network signet --esplora https://mutinynet.com/api --listen 0.0.0.0:9735 --canal-retour 100
# L'épargnant : ouvre un canal de 400 000 sats vers son fournisseur
cargo run --release -- run --role epargnant --data ~/menger-epargnant \
  --network signet --esplora https://mutinynet.com/api \
  --fournisseur CLE_DU_FOURNISSEUR@IP:9735 --canal 400000
```

L'application s'ouvre alors sur http://127.0.0.1:8421 (`--interface` pour changer l'adresse, `--interface aucune` pour s'en passer). Le programme lit aussi des commandes dans le terminal ; `aide` en donne la liste : `etat`, `stabiliser 200000`, `reprendre 100000`, `facture 0,3`, `payer FACTURE`, `annuler`, `retirer ADRESSE 50000`, `partir`, et pour un fournisseur `canal-public` et `fermer`.

Au premier lancement, le programme affiche une **phrase de récupération de 12 mots**, à écrire sur papier. Avec `--restaurer`, dans un dossier neuf, elle redonne accès aux bitcoins du portefeuille. Pour l'argent des canaux, il faut en plus sauvegarder le dossier `--data`, qui garde l'état de chaque canal.

Les réglages que chacun choisit pour lui-même :

| Option | Pour qui | Ce qu'elle règle |
|---|---|---|
| `--prices sources:FICHIER` | tous | Sa propre liste de sources de prix. `menger-ln sources` affiche la liste habituelle, à enregistrer et à modifier. |
| `--attente-hors-ligne HEURES` | tous | Le temps laissé à l'autre partie quand elle est injoignable (6 pour un épargnant, 72 pour un fournisseur). |
| `--rate-ppm 20000` | épargnant | Le prix de la stabilité qu'il propose : ici 2 % par an, payés au fournisseur. |
| `--ecart-max-ppm 3000` | épargnant | L'écart qu'il tolère entre son prix et celui du fournisseur quand sa position change : ici 0,3 %. |
| `--sans-stabilisation-auto` | épargnant | Garde en bitcoin ce qu'il reçoit. |
| `--taux-min-ppm`, `--marge-ppm` | fournisseur | Le prix de la stabilité le plus bas qu'il accepte, et sa marge sur chaque changement de position. |
| `--canal-retour 100` | fournisseur | Ouvre de lui-même, vers chaque épargnant, un canal de 100 % du sien (`--canal-retour-min`, `--canal-retour-max` pour les bornes). |
| `--nom NOM` | fournisseur | Le nom de son nœud, nécessaire pour ouvrir des canaux publics (`canal-public`) et relier ses épargnants au reste du réseau. |

## Limites connues

1. **La stabilité tient jusqu'au dernier paiement, pas au-delà.** Si le fournisseur cesse de payer pendant un krach, l'épargnant ferme ses canaux. Il récupère ses bitcoins au cours du moment, mais perd la baisse survenue depuis le dernier paiement : 10 minutes si le fournisseur est là et ne paie pas, jusqu'à 6 heures s'il a disparu. Une sortie forcée bloque en plus les fonds 144 blocs, environ un jour, et pendant ce temps ils ne sont plus stabilisés.
2. **Un épargnant absent a un avantage.** Tant qu'il est injoignable, le fournisseur ne peut pas encaisser ce qu'il lui doit ; s'il ne revient jamais, le fournisseur ferme et ne le récupère pas. Le fournisseur ne perd rien de ce qu'il détient, mais il renonce à un gain. C'est le prix de la stabilité, que chaque fournisseur fixe librement, qui paie ce risque.
3. **Le fournisseur fixe le prix des changements.** Il peut prendre, à chaque changement de position, jusqu'à l'écart que l'épargnant tolère (0,3 % par défaut). Chaque changement est écrit dans le journal avec le prix de l'épargnant à côté : un fournisseur qui en abuse se voit, et on le quitte. S'il fait échouer un paiement pour lequel la position a été réduite, l'épargnant repaie cet écart en remettant les sats dans la position. Il est aussi le premier intermédiaire de chaque paiement et peut prendre des frais jusqu'à la limite (1 % plus 50 sats).
4. **Le fournisseur doit avoir assez de bitcoins.** Avec autant de bitcoins que l'épargnant, il couvre une baisse de 50 % du bitcoin face à l'or.
5. **Une version de développement de LDK Node.** Le projet utilise LDK Node 0.8 dans son état du 10 novembre 2026, avant sa sortie officielle : elle repose sur LDK 0.3 (troisième version candidate), où un canal privé accepte des paiements aussi gros que lui, sans que l'épargnant ait à utiliser un canal non confirmé. La quatrième version candidate de LDK, sortie le 11 novembre 2026, corrige d'autres défauts mais ne compile pas encore avec un serveur Esplora. Il faudra passer à la version officielle dès sa sortie, et en tout cas avant de vrais bitcoins.
6. **Se connecter au moins une fois par jour.** Si le fournisseur publiait un ancien état d'un canal, le nœud de l'épargnant aurait 144 blocs, environ un jour, pour le punir. LDK permet d'allonger ce délai à deux semaines, mais LDK Node ne l'expose pas encore : c'est une petite contribution à proposer à ses auteurs. Aucun service de garde (« watchtower ») ne fonctionne encore avec LDK Node.
7. **Le fournisseur voit passer les paiements de ses épargnants**, comme tout nœud Lightning qui transmet un paiement. Il peut refuser d'en transmettre un, qui échoue alors (voir le point 3), mais il ne peut pas le détourner.
8. **Une ouverture de canal à la fois par portefeuille.** LDK Node ne met pas de côté les pièces d'une transaction d'ouverture qu'il signe : deux ouvertures rapprochées peuvent dépenser la même pièce, et la seconde ne serait jamais confirmée. Le programme attend donc que la précédente soit connue du portefeuille ; avec un nœud Bitcoin Core, qu'elle soit confirmée.
9. **Le canal en retour coûte au fournisseur.** Avec `--canal-retour`, n'importe qui peut lui faire ouvrir un canal en ouvrant le sien : le fournisseur immobilise des bitcoins et paie les frais d'ouverture. Une taille minimale limite l'abus ; l'option est éteinte par défaut.
10. **Le fournisseur se déclare service de liquidité LSPS2** (bLIP-52). Avec LDK Node, c'est ce qui lui permet de transmettre des paiements vers les canaux privés de ses épargnants. Les canaux à la demande de LSPS2 ne servent pas : personne ne reçoit le secret qu'il faut pour en demander un.
11. **L'application se lance depuis un terminal**, et toute personne qui utilise le même ordinateur peut l'ouvrir. Une application mobile, qui garderait les clés sur le téléphone, reste à faire.
12. **Les sources de l'or.** Trois familles doivent concorder à 1 % près, et deux suffisent : le comptant (Swissquote), le jeton PAXG (Kraken, Coinbase, Bitstamp, qui comptent pour une seule source) et le jeton XAUT (Kraken). Seuls des prix sont lus, rien n'est détenu. Une deuxième source au comptant serait préférable ; chacun peut ajouter les siennes dans son fichier de sources.
13. Les prix peuvent être lus à travers Tor : `ALL_PROXY=socks5://127.0.0.1:9050`.

## Fichiers

```
PROTOCOL.md   les règles et les messages, pour qui veut les relire ou écrire un autre programme
SECURITY.md   ce que chaque partie peut perdre, et contre qui le programme se défend
core/         les règles, sans réseau ni disque : unité, prix, stabilité, changements de position, messages
node/         le nœud Lightning (LDK Node), les sources de prix, la sauvegarde, le programme menger-ln
node/src/interface.html    l'application, une seule page ; web.rs la sert, api.rs fait le travail de ses boutons
node/src/keys.rs           la phrase de récupération
node/tests/regtest.rs      les tests de bout en bout sur un réseau Bitcoin privé
node/examples/mutinynet.rs un épargnant et un fournisseur sur Mutinynet, avec les vrais prix
```

## Licence

GPL v3, comme le reste du projet. Le code utilise [LDK Node](https://github.com/lightningdevkit/ldk-node) (MIT ou Apache 2.0).
