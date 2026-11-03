# Déployer Menger v0 sur un réseau de test

Ce guide se suit depuis ton ordinateur, dans un terminal (macOS ou Linux ; sous Windows, installe d'abord [WSL](https://learn.microsoft.com/fr-fr/windows/wsl/install) puis suis le guide dans Ubuntu).

> **Uniquement sur un réseau de test.** Le déploiement de test crée quatre sources de prix que **toi seul contrôles** : tu peux fixer le prix de l'ETH et de l'or à ta guise, pour les démonstrations. Avec de l'argent réel, ce serait une escroquerie possible. Le script refuse d'ailleurs de les déployer sur le vrai réseau Ethereum. Pour de l'argent réel, il faudra de vraies sources de prix, et un audit reste fortement conseillé.

Tout est fait pour fonctionner deux fois : d'abord en répétition sur ton ordinateur (gratuit, 10 minutes), puis sur Sepolia, le réseau de test public d'Ethereum (visible par tous, comme une démo en ligne). Les commandes de déploiement et de démonstration ont été jouées de bout en bout sur une blockchain locale le 10 octobre 2026, et l'interface web le 19 octobre 2026, avec un navigateur automatisé à la place de MetaMask. Seule la publication sur GitHub (section 4) n'a pas pu être essayée d'ici.

## 1. Installer les outils (une fois)

```bash
curl -L https://foundry.paradigm.xyz | bash
```

Ferme et rouvre le terminal, puis :

```bash
foundryup
```

Décompresse le projet, va dans son dossier et lance les tests. Le premier lancement télécharge le compilateur Solidity 0.6.11.

```bash
cd menger-v0
forge test
```

Tu dois voir `71 tests passed, 0 failed`.

## 2. Répétition générale sur ton ordinateur

Ouvre **un premier terminal** et lance une blockchain locale. Elle affiche 10 comptes de test, chacun avec 10 000 ETH fictifs, et leurs clés privées. Laisse ce terminal ouvert.

```bash
anvil
```

Dans **un second terminal**, dans le dossier du projet, prépare les réglages. Les deux clés ci-dessous sont celles des deux premiers comptes d'anvil : elles sont publiques et ne servent qu'en local.

```bash
export RPC_URL=http://127.0.0.1:8545
export MON_ADRESSE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
export ENVOI="--rpc-url $RPC_URL --broadcast --gas-estimate-multiplier 200 --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
export ENVOI2="--rpc-url $RPC_URL --broadcast --gas-estimate-multiplier 200 --private-key 0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
```

Puis déploie :

```bash
forge script script/DeployMenger.s.sol $ENVOI
```

Le script affiche les adresses des contrats et les enregistre dans `deployments/31337.json`. Les autres scripts les y relisent : tu n'as rien à recopier. Joue ensuite le scénario de la section 5 tel quel.

Sur anvil, tu peux avancer le temps, par exemple pour passer les 14 jours qui précèdent les rachats :

```bash
cast rpc evm_increaseTime 1296000 --rpc-url $RPC_URL && cast rpc evm_mine --rpc-url $RPC_URL
```

(1296000 secondes = 15 jours ; 86400 = 1 jour.) Si on tombe un week-end, avance encore d'un jour : `SetTestnetPrices` affiche `Redemptions allowed now true` quand les rachats sont ouverts. Si tu relances anvil, il repart de zéro : redéploie.

**L'interface web, en local.** Dans un troisième terminal, dans le dossier du projet :

```bash
python3 -m http.server 8000
```

Ouvre http://localhost:8000/app/?chain=31337 dans le navigateur où MetaMask est installé. Dans MetaMask, ajoute le réseau local (Réseaux, puis « Ajouter un réseau manuellement » : nom `Anvil`, URL RPC `http://127.0.0.1:8545`, ID de chaîne `31337`, symbole `ETH`), puis importe le compte de test avec la clé `0xac09…ff80` ci-dessus (« Importer un compte »). C'est le compte qui a déployé la démo : l'interface lui montre aussi les commandes de prix.

Si tu relances anvil, MetaMask garde l'historique de l'ancienne chaîne et ses transactions échouent : efface-le dans Paramètres, Avancé, « Clear activity tab data » (effacer les données de l'onglet Activité).

## 3. Déployer sur Sepolia

**Un portefeuille réservé aux tests.** N'utilise jamais un portefeuille qui contient de vrais fonds. Crée-en un :

```bash
cast wallet new
```

Note l'adresse (`Address`). Puis range la clé privée dans un fichier chiffré par mot de passe, sous le nom `menger-test` :

```bash
cast wallet import menger-test --interactive
```

Colle la clé privée affichée juste avant, puis choisis un mot de passe.

**De l'ETH de test.** Il en faut environ 0,3. Le déploiement consomme environ 23 millions de gaz, soit de 0,02 à 0,2 ETH selon l'encombrement du réseau. Le reste sert à ouvrir des emprunts de démonstration. Des robinets gratuits en distribuent, par exemple [celui de Google Cloud](https://cloud.google.com/application/web3/faucet/ethereum/sepolia) (connexion Google) ou [celui d'Alchemy](https://www.alchemy.com/faucets/ethereum-sepolia) (compte gratuit).

**Les réglages.** Le point d'accès public de PublicNode suffit pour commencer ; pour un usage régulier, un compte gratuit chez Alchemy ou Infura est plus fiable.

```bash
export RPC_URL=https://ethereum-sepolia-rpc.publicnode.com
export MON_ADRESSE=0x...   # l'adresse affichée par cast wallet new
export ENVOI="--rpc-url $RPC_URL --broadcast --gas-estimate-multiplier 200 --account menger-test"
```

`--gas-estimate-multiplier 200` double la marge de gaz. Le compilateur de Liquity, qui date de 2020, fait estimer le gaz selon les règles d'Ethereum de 2019, moins chères qu'aujourd'hui ; sans cette marge, certaines transactions échouent. Le gaz non utilisé n'est pas payé.

**Déployer.** Le terminal demande le mot de passe du portefeuille.

```bash
forge script script/DeployMenger.s.sol $ENVOI
```

Les adresses sont enregistrées dans `deployments/11155111.json` (11155111 est le numéro de Sepolia). Tu peux voir chaque contrat sur [sepolia.etherscan.io](https://sepolia.etherscan.io).

Options, à placer devant la commande :

- `ETH_USD_CENTS=4000000` démarre avec l'ETH à 40 000 $. Avec peu d'ETH de test, 0,2 ETH suffit alors pour emprunter 5 Mengers. Dans la démonstration ci-dessous, divise alors toutes les quantités d'ETH par 10, et les prix de krach aussi par rapport à 40 000 $.
- `XAU_USD_CENTS=431900` fixe le prix de départ de l'once d'or (4 319 $ par défaut).

**Publier le code source sur Etherscan (facultatif).** C'est utile pour que d'autres puissent lire les contrats. Il faut une clé d'API gratuite d'Etherscan : ajoute `--verify --etherscan-api-key TA_CLE` à la commande de déploiement.

## 4. Mettre l'interface en ligne

**Pour l'essayer toi-même**, lance le même petit serveur qu'en section 2 et ouvre http://localhost:8000/app/ : l'interface lit `deployments/11155111.json` et travaille sur Sepolia. Dans MetaMask, affiche les réseaux de test (Réseaux, « Afficher les réseaux de test »), passe sur Sepolia et importe ton portefeuille de test avec la clé privée affichée par `cast wallet new`. C'est lui qui a déployé les sources de prix de test : l'interface te montre donc les commandes de prix.

**Pour la montrer à tout le monde**, publie le projet sur GitHub, qui héberge gratuitement le site. Le code devient public, ce que la licence GPL v3 demande de toute façon. Crée un compte sur [github.com](https://github.com), installe l'outil `gh` ([cli.github.com](https://cli.github.com) ; sur Mac avec Homebrew : `brew install gh` ; dans Ubuntu : `sudo apt install gh`) et connecte-le :

```bash
gh auth login
```

Réponds GitHub.com, HTTPS, puis « Login with a web browser ». Ensuite, dans le dossier du projet, une fois le déploiement sur Sepolia fait :

```bash
git init -b main
git add .
git commit -m "Menger v0, démo sur Sepolia"
gh repo create menger-protocol --public --source=. --push
gh api -X POST "repos/{owner}/menger-protocol/pages" -f "source[branch]=main" -f "source[path]=/"
```

Une ou deux minutes plus tard, la démo est en ligne à `https://TON-PSEUDO.github.io/menger-protocol/` (`gh api user -q .login` affiche ton pseudo). Si `git commit` demande qui tu es, donne ton nom et l'adresse e-mail de ton compte GitHub avec `git config --global user.name "…"` et `git config --global user.email "…"`, puis relance la commande.

**Ton propre domaine (facultatif).** Si tu as acheté un domaine, par exemple `menger.cc`, indique-le dans le dépôt sur GitHub : Settings, Pages, « Custom domain ». Règle ensuite le DNS chez ton registraire comme GitHub l'indique. Ton lien ne dépendra plus de ton pseudo GitHub. GitHub ajoute au passage un fichier `CNAME` au dépôt : fais `git pull` avant ton prochain `git push`.

Le fichier `deployments/11155111.json` fait partie de ce que tu publies : c'est là que le site trouve les contrats. Après un nouveau déploiement, publie-le à nouveau :

```bash
git add deployments && git commit -m "Nouveau déploiement" && git push
```

Les visiteurs ont besoin de MetaMask et d'un peu d'ETH de test ; le site leur indique des robinets gratuits. Eux ne voient pas les commandes de prix.

## 5. Démonstration

Tout ce qui suit se fait aussi dans l'interface web, sans terminal : onglets Emprunter, Pool de stabilité et Racheter, liste des emprunts les plus risqués avec le bouton de liquidation, et commandes de prix pour le déployeur. Les commandes ci-dessous restent utiles pour vérifier ou automatiser.

Les montants s'écrivent en unités de 10⁻¹⁸ ; `$(cast to-wei 5)` écrit 5 Mengers ou 5 ETH dans ce format. Chaque action affiche ensuite l'état du système et de ta position.

**Voir l'état du système :**

```bash
forge script script/Demo.s.sol --sig "status()" --rpc-url $RPC_URL --sender $MON_ADRESSE
```

**Emprunter 5 Mengers contre 2 ETH :**

```bash
forge script script/Demo.s.sol --sig "openTrove(uint256,uint256)" $(cast to-wei 2) $(cast to-wei 5) $ENVOI
```

La dette est de 5,425 Mengers : les 5 empruntés, 0,5 % de frais et 0,4 Menger de réserve pour payer le gaz d'une éventuelle liquidation. La garantie doit valoir au moins 110 % de la dette, et la dette au moins 4 Mengers sans la réserve.

**Déposer 4 Mengers dans le pool de stabilité :**

```bash
forge script script/Demo.s.sol --sig "deposit(uint256)" $(cast to-wei 4) $ENVOI
```

**Provoquer une liquidation.** Il faut un second emprunt, risqué, ouvert depuis un second portefeuille. Sur anvil, `ENVOI2` est déjà prêt. Sur Sepolia, crée un second portefeuille (`cast wallet new`, puis `cast wallet import menger-test-2 --interactive`), envoie-lui un peu d'ETH de test, et prépare ses réglages :

```bash
export ENVOI2="--rpc-url $RPC_URL --broadcast --gas-estimate-multiplier 200 --account menger-test-2"
```

Ouvre avec lui un emprunt à environ 112 % : 0,75 ETH pour 5 Mengers, l'ETH étant à 4 000 $.

```bash
forge script script/Demo.s.sol --sig "openTrove(uint256,uint256)" $(cast to-wei 0.75) $(cast to-wei 5) $ENVOI2
```

Fais chuter l'ETH de 10 % (seul le déployeur peut changer les prix de test) :

```bash
ETH_USD_CENTS=360000 forge script script/SetTestnetPrices.s.sol $ENVOI
```

L'emprunt risqué passe sous 110 %. N'importe qui peut le liquider, et reçoit en récompense la réserve de 0,4 Menger et 0,5 % de la garantie :

```bash
forge script script/Demo.s.sol --sig "liquidate()" $ENVOI
```

Ton dépôt dans le pool a remboursé une partie de la dette, et tu as reçu de l'ETH en échange (`ETH gains to claim`). Avec seulement 4 Mengers dans le pool, il en garde 1 et le reste de la dette passe sur ton propre emprunt : c'est le filet de sécurité de Liquity. Pour encaisser tes gains sans retirer ton dépôt :

```bash
forge script script/Demo.s.sol --sig "withdraw(uint256)" 0 $ENVOI
```

**Racheter des Mengers contre de l'ETH**, au prix de l'or. Trois conditions : 14 jours après le déploiement, un jour de semaine (pas du vendredi 21 h au lundi 0 h UTC), et des prix à jour. Les sources de test ne se mettent pas à jour toutes seules : relance d'abord `SetTestnetPrices` sans variable, ce qui republie les mêmes prix avec l'heure actuelle.

```bash
forge script script/SetTestnetPrices.s.sol $ENVOI
forge script script/Demo.s.sol --sig "redeem(uint256)" $(cast to-wei 1) $ENVOI
```

**Changer le prix de l'or :**

```bash
XAU_USD_CENTS=480000 forge script script/SetTestnetPrices.s.sol $ENVOI
```

Si l'or monte, chaque Menger vaut plus de dollars : les dettes pèsent plus lourd et les garanties, en ETH, couvrent moins.

Le système considère qu'une source dont le prix saute de plus de 50 % d'un coup est en panne, comme Liquity V1. `SetTestnetPrices` publie donc les grands mouvements en plusieurs étapes, comme le ferait une vraie source (au plus -40 % ou +60 % à chaque fois) : `ETH_USD_CENTS=90000` fait bien passer l'ETH de 3 600 $ à 900 $.

**Voir tes Mengers dans MetaMask.** Active les réseaux de test pour voir Sepolia, importe ton portefeuille de test, puis « Importer des jetons » avec l'adresse `mengerToken` de `deployments/11155111.json`.

## 6. En cas d'erreur

| Message | Cause |
|---|---|
| `DRY RUN: nothing was deployed` | Il manque `--rpc-url` ou `--broadcast` : utilise `$ENVOI`. |
| `Transaction Failure` ou `out of gas` | Il manque `--gas-estimate-multiplier 200`. |
| `call to non-contract address` | Le fichier `deployments/` ne correspond pas au réseau : par exemple, anvil a été relancé. Redéploie. |
| `insufficient funds` | Pas assez d'ETH de test sur le portefeuille. |
| `ICR < MCR is not permitted` | La garantie ne couvre pas 110 % de la dette : mets plus d'ETH ou emprunte moins. |
| `net debt must be greater than minimum` | Il faut emprunter au moins 4 Mengers. |
| `would result in TCR < CCR` | L'opération ferait passer le ratio global du système sous 150 % (mode de récupération) : emprunte moins ou ajoute de l'ETH. |
| `must leave trove with ICR >= CCR` | Le système est en mode de récupération : un nouvel emprunt doit y être garanti à 150 % au moins. |
| `Redemptions are not allowed during bootstrap phase` | Moins de 14 jours depuis le déploiement. |
| `Redemptions suspended` | Week-end, ou prix de test pas mis à jour depuis plus de 4 h (ETH) ou 25 h (or) : relance `SetTestnetPrices`. |
| `Cannot redeem when TCR < MCR` | Le système entier est sous 110 % de garantie : les rachats attendent qu'il remonte. |
| `nothing to liquidate` | Aucun emprunt sous 110 %. |
| `Cannot withdraw while there are troves with ICR < MCR` | Il faut d'abord liquider les emprunts sous 110 % ; `withdraw(0)` reste possible. |
| `Withdrawal must leave totalBoldDeposits >= MIN_LUSD_IN_SP` | Le pool garde toujours au moins 1 Menger. |
| `forge: command not found` | Rouvre le terminal après l'installation, ou relance `foundryup`. |
| Interface : « Aucun déploiement trouvé » | Le site ne trouve pas `deployments/11155111.json` : déploie, puis publie ce fichier avec le site. |
| Interface : « Lecture de la blockchain impossible », sans portefeuille | Les points d'accès publics de Sepolia ne répondent pas : connecte MetaMask, l'interface lit alors par lui. |
| MetaMask : `nonce too high`, ou transactions refusées sur anvil | anvil a été relancé : efface l'historique dans MetaMask (Paramètres, Avancé, « Clear activity tab data »). |

Pour un déploiement avec de l'argent réel, ce guide ne suffit pas. Il faudra de vraies sources de prix (deux pour l'ETH, deux pour l'or, d'opérateurs différents), un robot de liquidation qui tourne en permanence, et une revue du paramètre `GOLD_MAX_AGE` selon la fréquence de mise à jour des sources d'or choisies. Un audit et un avis juridique restent fortement conseillés.
