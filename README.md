# Menger v0, prototype

Une monnaie stable en or, sans réserve d'or ni émetteur. **1 Menger = 3,545 g d'or fin**, le poids du ducat vénitien (1284-1797).

Le nom rend hommage à Carl Menger, le fondateur de l'école autrichienne. En 1892, dans *On the Origin of Money*, il a montré que la monnaie naît des échanges, sans l'État : le bien le plus facile à échanger s'impose, et c'est historiquement un métal précieux comme l'or. Et l'unité pèse un ducat : c'est le nom que Hayek donnait à la monnaie privée de son exemple, dans *Denationalisation of Money* (1976). Le Menger, c'est ce ducat-là, sans banque.

On emprunte des Mengers en déposant de l'ETH en garantie, comme on emprunte des LUSD sur Liquity. Le code est celui de Liquity V1, audité et en production depuis 2021, modifié le moins possible.

> **Version Bitcoin** : le dossier [`lightning/`](lightning/README.md) contient le Menger sur Lightning. C'est un solde stable en or, réglé en bitcoin, qu'on peut payer et recevoir, sans jeton ni oracle commun. C'est la direction choisie pour la suite. Ce qui suit décrit la v0 sur Ethereum, qui sert de démonstration.

> **Prototype de recherche, non audité.** À utiliser uniquement sur un réseau de test. Avant tout lancement avec de l'argent réel : audit indépendant et avis d'un avocat (voir `audit/AUDIT.md` et l'onglet « État du projet » du document de projet).

## Comment le Menger suit l'or

1. **Emprunter.** On dépose de l'ETH et on emprunte des Mengers. La garantie doit valoir au moins 110 % de la dette, mesurée en or.
2. **Plancher : le rachat.** N'importe qui peut échanger 1 Menger contre l'équivalent de 3,545 g d'or en ETH, moins un frais d'au moins 0,5 %. Si le Menger vaut moins que son or sur le marché, on l'achète et on le rachète avec profit, ce qui fait remonter son cours.
3. **Plafond : l'emprunt.** Si le Menger vaut plus que son or, on emprunte des Mengers pour les vendre, ce qui fait baisser son cours.
4. **Liquidation.** Un emprunt sous 110 % est liquidé. Le pool de stabilité rembourse la dette avec ses Mengers et reçoit l'ETH en échange, avec environ 10 % de marge.
5. **Prix.** Le prix de l'ETH en Mengers vaut ETH/USD ÷ (XAU/USD × 3,545 / 31,1034768). Deux sources donnent ETH/USD et deux autres le prix de l'once d'or (XAU/USD).

## Ce qui change par rapport à Liquity V1

| | Liquity V1 | Menger v0 |
|---|---|---|
| Unité de la dette | 1 dollar | 3,545 g d'or fin |
| Oracle | Chainlink, Tellor en secours (ETH/USD) | `MengerPriceFeed` : pour l'ETH comme pour l'or, une source principale et une de secours. La principale fait foi. La seconde la remplace si elle cesse d'être mise à jour, tombe en panne ou saute de plus de 50 % d'un coup ; une source qui a sauté reste écartée jusqu'à ce qu'elle concorde de nouveau avec l'autre (règle de Liquity V1). Ne bloque jamais les liquidations. |
| Rachats | Toujours ouverts | Suspendus du vendredi 21 h au lundi 0 h UTC (marché de l'or fermé). Suspendus aussi si les deux sources ne se confirment pas : écart de plus de 5 % pour l'ETH, de plus de 1 % pour l'or, ou or pas mis à jour récemment. |
| Jeton de gouvernance | LQTY | Aucun |
| Frais d'emprunt (en Mengers) | Aux détenteurs de LQTY | Aux déposants du pool de stabilité, versés progressivement |
| Frais de rachat (en ETH) | Aux détenteurs de LQTY | Aux déposants du pool, versés progressivement. Aucun frais si le pool est vide. |
| Période sans rachat au lancement | 14 jours après le jeton LQTY | 14 jours après le déploiement |
| Réserve de gaz, dette minimale | 200 et 1 800 LUSD | 0,4 et 4 Mengers (environ 200 et 2 000 $) |
| Gains des déposants | Payés tels quels | Payés avec un plafond : une erreur d'arrondi ne peut plus bloquer le dernier déposant |
| Administrateur | Aucun | Aucun (inchangé) |

Le détail ligne par ligne est dans `audit/liquity-v1-to-menger.diff`. Toutes les modifications sont marquées `MENGER:` dans le code. Les noms internes (`LUSD`, `LQTY`) sont gardés exprès, pour que les auditeurs comparent facilement avec Liquity V1.

**Le rythme de versement des frais.** À chaque opération du pool, il verse la part des frais en attente qui correspond au temps écoulé, rapporté à 7 jours. Si le pool est actif tous les jours, environ deux tiers d'un frais sont versés en une semaine et 96 % en trois semaines ; s'il ne se passe rien pendant 7 jours, tout est versé à l'opération suivante. Versé d'un coup, un gros frais pouvait être capté par quelqu'un qui déposait juste avant et retirait juste après : environ 90 % du frais dans nos tests, sans avoir protégé le système. Désormais ce dépôt éclair ne touche rien. En revanche, un gros déposant qui reste des semaines dans le pool récupère, comme tout déposant, une part des frais égale à sa part du pool, y compris de ses propres frais.

## Lancer les tests

Il faut [Foundry](https://book.getfoundry.sh/getting-started/installation). Le compilateur Solidity 0.6.11 se télécharge tout seul au premier lancement.

```bash
forge test                  # les 71 tests, environ 30 secondes
forge test -vv              # avec le détail

# Tests aléatoires plus longs (environ 8 minutes) :
FOUNDRY_INVARIANT_RUNS=1000 FOUNDRY_INVARIANT_DEPTH=200 forge test --match-contract MengerInvariantTest
FOUNDRY_FUZZ_RUNS=5000 forge test --match-test testFuzz
```

Les simulations sur l'historique ont besoin de Python 3, sans module supplémentaire :

```bash
python3 sim/history.py      # télécharge l'historique, choisit les pires krachs, écrit test/sim/CrashData.sol
python3 sim/report.py       # rejoue ces krachs avec les vrais contrats, écrit sim/out/crash_replay.md
```

## L'interface web

`app/` est un site sans serveur ni compilation : une page qui lit les contrats et fait signer chaque action par MetaMask. On y emprunte, on dépose dans le pool de stabilité, on rachète et on liquide ; le déployeur y fixe aussi les prix de test. Elle existe en français et en anglais, et s'adapte au téléphone et au mode sombre.

```bash
python3 -m http.server 8000   # puis http://localhost:8000/app/ (Sepolia) ou /app/?chain=31337 (anvil)
```

Pour la mettre en ligne gratuitement sur GitHub Pages, voir `DEPLOIEMENT.md`, section 4. Elle a été testée de bout en bout le 19 octobre 2026 sur une blockchain locale, avec un navigateur automatisé à la place de MetaMask : ouvrir, modifier et fermer un emprunt, déposer, faire chuter l'ETH, liquider, encaisser les gains, racheter, et des opérations refusées dont l'erreur est expliquée. Une relecture indépendante de l'interface a relevé 18 défauts, aucun grave (montants mal lus comme « 1,000 » en anglais, frais d'emprunt maximal trop large, page bloquée si MetaMask accélère une transaction, mode de récupération mal signalé…) : tous sont corrigés, et les principaux ont leur test.

## Résultats au 10 octobre 2026

**71 tests, tous réussis.**

- 23 tests de l'oracle : conversion en or (sur 5 000 couples de prix aléatoires), week-end, sources figées, en panne, en désaccord, qui sautent, hostiles ou qui renvoient des valeurs absurdes, et 5 000 combinaisons aléatoires de réponses sans qu'aucune ne bloque le prix.
- 21 tests des emprunts, rachats, liquidations et frais.
- 9 tests du versement progressif des frais.
- 6 tests qui rejouent les problèmes trouvés par les tests aléatoires et par la relecture.
- 9 règles vérifiées après chaque action, sur 1 000 séquences aléatoires de 200 actions chacune : dépôts, emprunts, krachs, sources de prix figées, décalées ou cassées, liquidations, rachats, passage du temps. Plus 1 test qui contrôle le générateur de ces actions.
- 2 rejeux des pires krachs historiques.

**Trois séries de problèmes trouvés et corrigés.**

1. *Par les tests aléatoires.* Une grosse liquidation peut vider le pool de stabilité jusqu'à son minimum d'1 Menger. Un gros frais réparti sur ce seul Menger amplifiait alors une erreur d'arrondi, et le retrait du dernier déposant échouait : son dépôt restait bloqué. Désormais les gains sont payés avec un plafond, et le dernier déposant reçoit une poussière en moins (de l'ordre de 10⁻¹⁵ ETH).
2. *Par une relecture indépendante du code*, faite par une autre instance de Claude qui n'avait pas vu le code s'écrire. Elle a trouvé huit points, chacun démontré par un test ou par un essai sur une blockchain locale. Les principaux :
   - une source ETH figée sur un ancien prix pouvait figer tout le système pendant un krach : plus de liquidations, et des emprunts au-delà de la valeur de la garantie ;
   - une source d'or abandonnée pouvait figer le prix de l'or pour toujours ;
   - une valeur absurde renvoyée par une source pouvait bloquer les liquidations ;
   - un défaut hérité de Liquity laissait 1 unité bloquée chez le dernier déposant passé par une interface ;
   - les scripts de déploiement manquaient de gaz sur un réseau actuel.

   Tout est corrigé, et chaque correction a son test.
3. *Par une seconde relecture*, des corrections elles-mêmes, par la même instance. Elle a trouvé quatre nouveaux points dans l'oracle réécrit, tous corrigés et testés :
   - une source principale cassée redevenait crue dès qu'elle répétait sa valeur fausse : elle reste maintenant écartée tant qu'elle ne concorde pas avec l'autre ;
   - le week-end, la source d'or secondaire pouvait à elle seule changer le prix de l'or ;
   - une source sans historique des cotations était ignorée sans avertissement : elle est maintenant refusée au déploiement ;
   - une source hostile pouvait encore épuiser le gaz de la lecture du prix.

Restent des propriétés économiques, décrites dans les limites ci-dessous (points 4 à 6).

**Les 12 pires krachs de l'ETH mesuré en or**, rejoués jour par jour avec les vrais contrats (2016-2022 ; rien de comparable de 2023 à mai 2026). Le test place 40 emprunteurs, de 115 % à 400 % de garantie, qui ne réagissent jamais, et un robot qui liquide une fois par jour. C'est un pire cas : en réalité les robots liquident en quelques minutes.

| | Pool de stabilité à 50 % des Mengers | Pool à 15 % |
|---|---|---|
| Système resté solvable | 12 krachs sur 12 | 12 sur 12 |
| Ratio global le plus bas | 111 % (mars 2020) | 106 % (novembre 2018) |
| Résultat du pool au prix du jour | Positif dans les 12 krachs (+0,3 % à +7,5 %) | Négatif dans 3 krachs (jusqu'à -3,3 %) |
| Dette transférée aux emprunteurs restants | Jusqu'à 33 % de la dette initiale | Jusqu'à 83 % |

Le détail est dans `sim/out/crash_replay.md` et `sim/out/history_stats.md`. Le week-end, l'oracle garde le prix de l'or du vendredi. Entre vendredi et lundi, l'or a bougé de 0,56 % en médiane, de 3,4 % au 99e centile et de 7,6 % au pire (mars 2020). La marge entre 110 % et 100 % de garantie absorbe ces écarts.

## Limites connues

1. **Non audité.** Le pool de stabilité de la version de Liquity utilisée (branche `main` de novembre 2025) diffère de celui déployé en 2021 : il garde toujours au moins 1 Menger, environ 490 $, que le dernier déposant ne peut pas retirer. Il fait partie du périmètre d'audit.
2. **La source principale fait foi.** Si elle donne un prix faux mais plausible (moins de 50 % d'écart d'un coup), ou si elle se fige en continuant d'horodater, le système la suit ; la source de secours ne fait alors que suspendre les rachats. C'est le choix de Liquity V1 avec Chainlink. Autre limite partagée avec Liquity V1 : un saut n'est repéré que si le prix est lu pendant ce tour-là ; un robot qui appelle `fetchPrice()` à chaque mise à jour des sources ferme cette porte. Les sources réelles, d'opérateurs différents et qui donnent leur historique (`getRoundData`), restent à choisir.
3. **Jours fériés.** Les sources de prix republient le même prix à intervalle fixe même marché fermé. Un jour férié en semaine peut donc avoir l'air « frais » et laisser passer des rachats à un prix figé. Le frais de rachat d'au moins 0,5 % limite l'intérêt de la manœuvre.
4. **Les gros déposants récupèrent une partie de leurs propres frais.** Un emprunteur ou racheteur qui détient l'essentiel du pool et y reste plusieurs semaines récupère l'essentiel de ses frais. Le frais minimal de 0,5 % dissuade donc moins les gros acteurs. Liquity V2 a choisi une autre voie : le frais de rachat revient aux emprunteurs rachetés. À trancher avec les auditeurs.
5. **Les frais arrivent progressivement**, à peu près sur trois semaines (voir plus haut). Un frais d'emprunt ne commence à être versé qu'à la prochaine opération du pool.
6. **Rachats gratuits tant que le pool est vide.** Ils sont de toute façon bloqués les 14 premiers jours. Si le pool est encore vide ensuite, le premier dépôt, même minuscule, fait revenir les frais et en touche la totalité.
7. **Rémunération des interfaces** (« front-end kickback ») gardée de Liquity : une interface peut toucher une part des frais d'emprunt de ses déposants. À valider juridiquement avant toute utilisation.
8. **Nom** : Menger, symbole MNGR (le projet s'appelait Ducat jusqu'en novembre 2026). Aucune recherche de marque n'a été faite. La crypto Scala a donné le nom Menger à une de ses mises à jour en 2021, et un jeton peu connu, Mongraal, utilise le symbole MNGR.
9. **Coût en gaz.** Chaque lecture du prix interroge quatre sources deux fois (prix et tour précédent) : de l'ordre de 100 000 unités de gaz, à chaque emprunt, rachat, retrait du pool ou liquidation.
10. **Les tests ne remplacent pas un audit**, et la simulation repose sur des hypothèses : emprunteurs passifs, prix journaliers, répartition des emprunts choisie par nous.

## Fichiers

```
src/                    les contrats (Liquity V1 modifié + MengerPriceFeed + StabilityPoolFeeIssuance)
test/                   les tests Foundry ; test/sim/ rejoue les krachs historiques
script/                 déploiement sur un réseau de test et actions de démonstration
sim/                    analyse de l'historique ETH/or (Python) et résultats dans sim/out/
audit/                  rapport pour les auditeurs (anglais) et diff complet avec Liquity V1
app/                    l'interface web (HTML, CSS, JavaScript ; ethers.js et les polices Alegreya inclus)
index.html              renvoie vers app/ quand le projet est publié sur GitHub Pages
DEPLOIEMENT.md          déployer sur un réseau de test depuis ton ordinateur, puis mettre l'interface en ligne
```

## Licence et sources

Le code de départ est [Liquity V1](https://github.com/liquity/dev), commit `3e64ee1b` (25 novembre 2025), © Liquity AG. Les fichiers portent la mention MIT, mais le dépôt et son README déclarent la licence **GNU GPL v3** : ce dérivé est donc distribué sous GPL v3 (fichier `LICENSE`). Il doit rester open source, et les modifications sont signalées (`MENGER:` et le diff). À faire confirmer par l'avocat.

L'interface inclut [ethers.js](https://github.com/ethers-io/ethers.js) 6.17 (licence MIT) et les polices [Alegreya et Alegreya Sans](https://github.com/huertatipografica/Alegreya) (SIL Open Font License 1.1), avec leurs licences dans `app/vendor/` et `app/fonts/`.

Données de prix : [Coin Metrics Community Data](https://github.com/coinmetrics/data) (ETH, PAXG, XAUT ; licence CC BY-NC 4.0, usage non commercial) et [datasets/gold-prices](https://github.com/datasets/gold-prices) (moyennes mensuelles de la Banque mondiale, domaine public). Elles sont téléchargées par `sim/history.py` et ne sont pas redistribuées ; seuls les prix journaliers des 12 fenêtres de krach figurent dans `test/sim/CrashData.sol`.
