// Menger v0 demo interface. Plain JavaScript, no build step: reads the contracts with ethers v6 and sends every
// action as a transaction from the visitor's wallet. Contract addresses come from deployments/<chain id>.json,
// written by script/DeployMenger.s.sol. Nothing is stored anywhere but in the visitor's browser (language choice).
(() => {
  "use strict";

  const E = window.ethers;
  const ZERO = E.ZeroAddress;
  const WAD = 10n ** 18n;
  const GAS_COMP = 4n * 10n ** 17n;       // 0.4 unit kept as gas reserve (LUSD_GAS_COMPENSATION)
  const MIN_NET_DEBT = 4n * WAD;          // 4 units
  const MIN_IN_SP = WAD;                  // the Stability Pool always keeps 1 unit
  const MCR = 11n * 10n ** 17n;           // 110 %
  const CCR = 15n * 10n ** 17n;           // 150 %
  const FEE_FLOOR = 5n * 10n ** 15n;      // 0.5 %, floor of both fees
  const SLIPPAGE = 5n * 10n ** 15n;       // the fee may rise by 0.5 point between preview and mining
  const BOOTSTRAP = 14n * 86400n;
  const UNIT_GRAMS = 35450000n;           // 3.545 g, in 1e-7 g
  const OUNCE_GRAMS = 311034768n;         // 31.1034768 g
  const DEFAULT_CHAIN = 11155111;
  const MANY_STEPS = 6;                   // ask before sending more demo transactions than this

  const NETWORKS = {
    11155111: {
      name: "Sepolia", hex: "0xaa36a7",
      rpcs: ["https://ethereum-sepolia-rpc.publicnode.com", "https://sepolia.drpc.org", "https://1rpc.io/sepolia"],
      explorer: "https://sepolia.etherscan.io", faucets: true,
      add: { chainId: "0xaa36a7", chainName: "Sepolia", nativeCurrency: { name: "Sepolia ETH", symbol: "ETH", decimals: 18 },
             rpcUrls: ["https://ethereum-sepolia-rpc.publicnode.com"], blockExplorerUrls: ["https://sepolia.etherscan.io"] }
    },
    31337: {
      name: "Anvil", hex: "0x7a69", rpcs: ["http://127.0.0.1:8545"], explorer: null, faucets: false,
      add: { chainId: "0x7a69", chainName: "Anvil", nativeCurrency: { name: "ETH", symbol: "ETH", decimals: 18 },
             rpcUrls: ["http://127.0.0.1:8545"] }
    }
  };

  const ABI = {
    token: [
      "function name() view returns (string)", "function symbol() view returns (string)",
      "function totalSupply() view returns (uint256)", "function balanceOf(address) view returns (uint256)"
    ],
    priceFeed: [
      "function fetchPrice() returns (uint256)",
      "function getEthUsdPrice() view returns (uint256 price, bool reliable)",
      "function getXauUsdPrice() view returns (uint256 price, bool agreed, bool fresh)",
      "function isRedemptionAllowed() view returns (bool)", "function isGoldMarketClosedAt(uint256) pure returns (bool)"
    ],
    troveManager: [
      "function getEntireSystemColl() view returns (uint256)", "function getEntireSystemDebt() view returns (uint256)",
      "function getTCR(uint256) view returns (uint256)", "function checkRecoveryMode(uint256) view returns (bool)",
      "function getTroveOwnersCount() view returns (uint256)", "function getBorrowingRateWithDecay() view returns (uint256)",
      "function getRedemptionRateWithDecay() view returns (uint256)", "function systemDeploymentTime() view returns (uint256)",
      "function getTroveStatus(address) view returns (uint256)",
      "function getEntireDebtAndColl(address) view returns (uint256 debt, uint256 coll, uint256 pendingLUSDDebtReward, uint256 pendingETHReward)",
      "function getCurrentICR(address, uint256) view returns (uint256)",
      "function liquidateTroves(uint256)",
      "function redeemCollateral(uint256, address, address, address, uint256, uint256, uint256)",
      "event Redemption(uint256 _attemptedLUSDAmount, uint256 _actualLUSDAmount, uint256 _ETHSent, uint256 _ETHFee)"
    ],
    borrowerOperations: [
      "function openTrove(uint256, uint256, address, address) payable",
      "function adjustTrove(uint256, uint256, uint256, bool, address, address) payable",
      "function closeTrove()", "function claimCollateral()"
    ],
    stabilityPool: [
      "function getTotalLUSDDeposits() view returns (uint256)", "function getCompoundedLUSDDeposit(address) view returns (uint256)",
      "function getDepositorETHGain(address) view returns (uint256)", "function getDepositorLQTYGain(address) view returns (uint256)",
      "function provideToSP(uint256, address)", "function withdrawFromSP(uint256)", "function withdrawETHGainToTrove(address, address)"
    ],
    sortedTroves: ["function findInsertPosition(uint256, address, address) view returns (address, address)"],
    hintHelpers: [
      "function getApproxHint(uint256, uint256, uint256) view returns (address hintAddress, uint256 diff, uint256 latestRandomSeed)",
      "function getRedemptionHints(uint256, uint256, uint256) view returns (address firstRedemptionHint, uint256 partialRedemptionHintNICR, uint256 truncatedLUSDamount)"
    ],
    multiTroveGetter: [
      "function getMultipleSortedTroves(int256, uint256) view returns (tuple(address owner, uint256 debt, uint256 coll, uint256 stake, uint256 snapshotETH, uint256 snapshotLUSDDebt)[])"
    ],
    collSurplusPool: ["function getCollateral(address) view returns (uint256)"],
    feed: [
      "function latestRoundData() view returns (uint80, int256, uint256, uint256, uint80)",
      "function owner() view returns (address)", "function answer() view returns (int256)",
      "function setAnswer(int256)", "function refresh()"
    ]
  };

  const params = new URLSearchParams(location.search);
  const target = Number(params.get("chain")) || DEFAULT_CHAIN;

  const S = {
    lang: "fr",
    target,                                // the network this page works on
    net: NETWORKS[target] || { name: `#${target}`, rpcs: [], explorer: null, faucets: false },
    dep: null,
    read: null,                            // provider used for reading
    signer: null,
    account: null,
    onTarget: false,                       // wallet connected and on the target network
    c: null,                               // contracts for reading
    w: null,                               // contracts for sending transactions
    data: null,
    tab: "borrow",
    busy: false,
    refreshing: null,
    refreshAgain: false,
    timer: null,
    retry: null,
    tokenName: "Menger",
    tokenSymbol: "MNGR",
    demoPrefilled: false,
    redeemCheck: { seq: 0, amount: null, truncated: null },
    redeemTimer: null
  };

  const $ = (id) => document.getElementById(id);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  // --- Language ---

  function loadLang() {
    try {
      const saved = localStorage.getItem("menger-lang");
      if (saved === "fr" || saved === "en") { return saved; }
    } catch (e) { /* storage unavailable */ }
    return (navigator.language || "fr").toLowerCase().startsWith("fr") ? "fr" : "en";
  }

  function saveLang(lang) {
    try { localStorage.setItem("menger-lang", lang); } catch (e) { /* storage unavailable */ }
  }

  function plural(name) { return /[sxz]$/i.test(name) ? name : name + "s"; }

  function t(key, vars) {
    const dict = window.I18N[S.lang] || window.I18N.fr;
    let s = dict[key] !== undefined ? dict[key] : (window.I18N.fr[key] !== undefined ? window.I18N.fr[key] : key);
    const all = Object.assign({
      name: S.tokenName, names: plural(S.tokenName), symbol: S.tokenSymbol, network: S.net.name
    }, vars || {});
    s = s.replace(/\{(\w+)\}/g, (m, k) => (all[k] !== undefined ? String(all[k]) : m));
    if (S.lang === "fr") {
      // French spacing: no-break space before : ; ? ! % and inside guillemets
      s = s.replace(/ ([:;?!%»])/g, " $1").replace(/« /g, "« ");
    }
    return s;
  }

  function applyStatic() {
    document.documentElement.lang = S.lang;
    document.querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); });
    document.querySelectorAll("[data-i18n-aria]").forEach((el) => { el.setAttribute("aria-label", t(el.dataset.i18nAria)); });
    document.querySelectorAll(".lang button").forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.lang === S.lang)));
    document.querySelectorAll(".sym").forEach((el) => { el.textContent = S.tokenSymbol; });
    document.title = S.lang === "fr" ? `${S.tokenName}, démo` : `${S.tokenName} demo`;
    $("brandName").textContent = S.tokenName;
    $("heroUnit").textContent = t("hero.unit");
    $("heroWeight").textContent = t("hero.weight");
    $("heroFine").textContent = t("hero.fine");
    $("heroHow").textContent = t("hero.how");
    $("balTokLabel").textContent = S.tokenSymbol;
    $("watchBtn").textContent = t("account.watch");
    $("openHelp").textContent = t("open.help");
    $("poolIntro").textContent = t("pool.intro");
    $("poolFeeGainLabel").textContent = t("pool.feeGain");
    $("redeemIntro").textContent = t("redeem.intro");
    $("sysUnitUsdLabel").textContent = t("system.unitUsd");
    const testnet = !S.dep || S.dep.testnetFeeds !== false;
    $("banner").textContent = testnet ? t("banner") : t("banner.real");
    $("demoRefreshNote").textContent = t("demo.refreshNote", { hours: Math.round(Number((S.dep && S.dep.goldMaxAge) || 90000) / 3600) });
  }

  // --- Formatting ---

  const locale = () => (S.lang === "fr" ? "fr-CH" : "en-US");
  function num(x, digits) {
    return new Intl.NumberFormat(locale(), { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(x);
  }
  const toN = (b, dec) => Number(E.formatUnits(b, dec === undefined ? 18 : dec));
  const fmtTok = (b, d) => `${num(toN(b), d === undefined ? 2 : d)} ${S.tokenSymbol}`;
  const fmtEth = (b, d) => `${num(toN(b), d === undefined ? 4 : d)} ETH`;
  const fmtUsd = (x) => (S.lang === "fr" ? `${num(x, 2)} $` : `$${num(x, 2)}`);
  const fmtPct = (b, d) => `${num(toN(b) * 100, d === undefined ? 1 : d)}${S.lang === "fr" ? " %" : "%"}`;
  const short = (a) => `${a.slice(0, 6)}…${a.slice(-4)}`;

  function fmtDuration(seconds) {
    const s = Number(seconds);
    if (s < 3600) { return t("unit.min", { n: Math.max(1, Math.round(s / 60)) }); }
    if (s < 2 * 86400) { return t("unit.h", { n: Math.round(s / 3600) }); }
    return t("unit.d", { n: Math.round(s / 86400) });
  }

  function ratioClass(icr) {
    if (icr < MCR) { return "bad"; }
    if (icr < CCR) { return "risk"; }
    return "good";
  }

  // --- Reading numbers typed by the visitor ---

  // Returns a plain "1234.5" string, or null. Spaces and apostrophes group thousands. When both "," and "." appear,
  // the last one is the decimal separator. A lone "," is a thousands separator only in English and only in groups
  // of three ("1,000"); otherwise it is a decimal comma ("0,5", or "1,000" in French).
  function normalizeNumber(str, lang) {
    let s = String(str || "").trim().replace(/[\s  '’_]/g, "");
    if (s === "") { return null; }
    const comma = s.lastIndexOf(",");
    const dot = s.lastIndexOf(".");
    if (comma >= 0 && dot >= 0) {
      s = comma > dot ? s.replace(/\./g, "").replace(",", ".") : s.replace(/,/g, "");
    } else if (comma >= 0) {
      if ((lang || S.lang) === "en" && /^\d{1,3}(,\d{3})+$/.test(s)) { s = s.replace(/,/g, ""); }
      else if ((s.match(/,/g) || []).length === 1) { s = s.replace(",", "."); }
      else { return null; }
    } else if ((s.match(/\./g) || []).length > 1) {
      if (/^\d{1,3}(\.\d{3})+$/.test(s)) { s = s.replace(/\./g, ""); } else { return null; }
    }
    if (!/^\d*\.?\d*$/.test(s) || s === ".") { return null; }
    if (s.endsWith(".")) { s = s.slice(0, -1); }
    if (s.startsWith(".")) { s = "0" + s; }
    return s;
  }

  // { value } with a positive bigint, or { error } with the text to show
  function readNumber(str, decimals, lang) {
    const s = normalizeNumber(str, lang);
    if (s === null) { return { error: t("err.amount") }; }
    const frac = s.includes(".") ? s.split(".")[1] : "";
    if (frac.length > decimals) { return { error: t("err.decimals", { n: decimals }) }; }
    let v;
    try { v = E.parseUnits(s, decimals); } catch (e) { return { error: t("err.amount") }; }
    return v > 0n ? { value: v } : { error: t("err.amount") };
  }

  const readAmount = (id) => readNumber($(id).value, 18);

  // The exact value, written back into an input in the page's language, without thousands separators
  function inputValue(b, decimals) {
    let s = E.formatUnits(b, decimals === undefined ? 18 : decimals);
    if (s.endsWith(".0")) { s = s.slice(0, -2); }
    return S.lang === "fr" ? s.replace(".", ",") : s;
  }

  // --- Notices and transaction status ---

  function showNotice(text, isError) {
    const el = $("globalNotice");
    el.textContent = text || "";
    el.hidden = !text;
    el.classList.toggle("error", !!isError);
  }

  function setStatus(el, kind, text, hash) {
    el.className = "form-status" + (kind ? " " + kind : "");
    el.textContent = text || "";
    if (hash && S.net.explorer) {
      const a = document.createElement("a");
      a.href = `${S.net.explorer}/tx/${hash}`;
      a.target = "_blank";
      a.rel = "noopener";
      a.textContent = t("tx.view");
      el.append(" ", a);
    }
  }

  function explain(e) {
    if (e && e.code === "REPLACED") { return t("tx.replaced"); }
    if (e && e.code === "LOCAL") { return e.message; }
    const code = e && (e.code !== undefined ? e.code : (e.info && e.info.error && e.info.error.code));
    if (code === "ACTION_REJECTED" || code === 4001) { return t("tx.rejected"); }
    const parts = [];
    const add = (x) => { if (x && typeof x === "string") { parts.push(x); } };
    if (e) {
      add(e.reason); add(e.shortMessage); add(e.message);
      if (e.revert && e.revert.args) { add(String(e.revert.args[0])); }
      if (e.info && e.info.error) { add(e.info.error.message); }
      if (e.error) { add(e.error.message); }
    }
    const raw = parts.join(" | ");
    if (/user (rejected|denied)/i.test(raw)) { return t("tx.rejected"); }
    for (const [fragment, key] of window.REVERTS) {
      if (raw.includes(fragment)) { return t("tx.failed", { reason: t(key) }); }
    }
    const first = (e && (e.reason || e.shortMessage || e.message)) || String(e);
    return t("tx.failed", { reason: first.slice(0, 180) });
  }

  function localError(message) {
    const e = new Error(message);
    e.code = "LOCAL";
    return e;
  }

  // Sends a contract call with gas headroom. The estimate is exact for the state at estimation time, but the
  // state can change before the transaction is mined (fees are released by the second, hints may need a longer
  // search), and a transaction with exactly the estimated gas then runs out of gas.
  async function send(contract, method, args, overrides) {
    const fn = contract.getFunction(method);
    const opts = Object.assign({}, overrides || {});
    const estimate = await fn.estimateGas(...args, opts);
    opts.gasLimit = (estimate * 12n) / 10n + 60000n;
    return fn(...args, opts);
  }

  // Waits for the receipt. If the wallet replaced the transaction (speed up, cancel), its nonce is used by another
  // transaction and this hash will never be mined: stop waiting and let the refreshed page show what happened.
  async function waitMined(tx) {
    const pause = S.target === 31337 ? 400 : 3000;
    for (;;) {
      const receipt = await S.read.getTransactionReceipt(tx.hash);
      if (receipt) { return receipt; }
      const used = await S.read.getTransactionCount(S.account, "latest");
      if (used > tx.nonce) {
        const again = await S.read.getTransactionReceipt(tx.hash);
        if (again) { return again; }
        const err = new Error("transaction replaced");
        err.code = "REPLACED";
        throw err;
      }
      await sleep(pause);
    }
  }

  // Sends the transactions one after the other, waiting for each to be mined, then reloads the state. The result is
  // shown once the page reflects it, in doneEl if the action hides its own form.
  async function runTxs(statusEl, steps, done, opts) {
    if (S.busy || !S.onTarget) { return null; }
    opts = opts || {};
    const doneEl = opts.doneEl || statusEl;
    const focused = document.activeElement;
    S.busy = true;
    render();
    const receipts = [];
    let lastHash = null;
    let error = null;
    try {
      for (let i = 0; i < steps.length; i++) {
        setStatus(statusEl, "pending", steps.length > 1 ? t("demo.progress", { i: i + 1, n: steps.length }) : t("tx.confirm"));
        const tx = await steps[i]();
        lastHash = tx.hash;
        setStatus(statusEl, "pending", t("tx.pending"), tx.hash);
        const receipt = await waitMined(tx);
        if (receipt.status !== 1) { throw new Error("transaction execution reverted"); }
        receipts.push(receipt);
      }
    } catch (e) {
      console.error(e);
      error = e;
    }
    if (!error && opts.clear) { opts.clear(); }
    S.busy = false;
    await refresh();
    if (error) {
      // The action may have changed what is shown (a replaced transaction that went through): say it where it is seen
      const target = statusEl.offsetParent === null && doneEl.offsetParent !== null ? doneEl : statusEl;
      if (target !== statusEl) { setStatus(statusEl, "", ""); }
      setStatus(target, "error", explain(error));
    } else {
      if (doneEl !== statusEl) { setStatus(statusEl, "", ""); }
      setStatus(doneEl, "done", typeof done === "function" ? done(receipts) : t(done), lastHash);
      // Some nodes answer reads from a block or two behind: read again a moment later
      setTimeout(() => { if (!S.busy) { refresh(); } }, 4000);
    }
    restoreFocus(focused, error ? statusEl : doneEl);
    return error ? null : receipts;
  }

  function restoreFocus(el, fallback) {
    if (el && el !== document.body && document.body.contains(el) && !el.disabled && el.offsetParent !== null) {
      el.focus();
      return;
    }
    if (fallback.offsetParent !== null) {
      fallback.tabIndex = -1;
      fallback.focus();
    }
  }

  // --- Contracts and hints ---

  function contracts(runner) {
    const d = S.dep;
    const mk = (addr, abi) => new E.Contract(addr, abi, runner);
    return {
      token: mk(d.mengerToken, ABI.token),
      priceFeed: mk(d.priceFeed, ABI.priceFeed),
      troveManager: mk(d.troveManager, ABI.troveManager),
      borrowerOperations: mk(d.borrowerOperations, ABI.borrowerOperations),
      stabilityPool: mk(d.stabilityPool, ABI.stabilityPool),
      sortedTroves: mk(d.sortedTroves, ABI.sortedTroves),
      hintHelpers: mk(d.hintHelpers, ABI.hintHelpers),
      multiTroveGetter: mk(d.multiTroveGetter, ABI.multiTroveGetter),
      collSurplusPool: mk(d.collSurplusPool, ABI.collSurplusPool),
      feeds: [d.ethUsdPrimary, d.ethUsdSecondary, d.xauUsdPrimary, d.xauUsdSecondary].map((a) => mk(a, ABI.feed))
    };
  }

  // Nominal collateral ratio, as SortedTroves orders troves
  const nicr = (coll, debt) => (debt > 0n ? (coll * 10n ** 20n) / debt : 0n);

  // Where a trove with this NICR belongs in the sorted list (Liquity's recommended hint procedure)
  async function insertHints(n) {
    const count = Number(await S.c.troveManager.getTroveOwnersCount());
    if (count === 0) { return [ZERO, ZERO]; }
    const trials = BigInt(Math.max(15, Math.ceil(15 * Math.sqrt(count))));
    const seed = BigInt(Math.floor(Math.random() * 2147483647));
    const approx = await S.c.hintHelpers.getApproxHint(n, trials, seed);
    const pos = await S.c.sortedTroves.findInsertPosition(n, approx[0], approx[0]);
    return [pos[0], pos[1]];
  }

  // The highest borrowing fee the visitor accepts: the previewed rate, plus a little in case it rises before mining
  function maxBorrowFee() {
    const rate = S.data && !S.data.recovery ? S.data.borrowRate : FEE_FLOOR;
    let max = rate + SLIPPAGE;
    if (max < FEE_FLOOR) { max = FEE_FLOOR; }
    return max > WAD ? WAD : max;
  }

  // --- Loading ---

  async function loadDeployment(chainId) {
    const paths = [`../deployments/${chainId}.json`, `deployments/${chainId}.json`];
    if (!/\/app\/(index\.html)?$/.test(location.pathname)) { paths.reverse(); }
    for (const path of paths) {
      try {
        const r = await fetch(path, { cache: "no-store" });
        if (r.ok) { return await r.json(); }
      } catch (e) { /* try the next path */ }
    }
    return null;
  }

  // Without a wallet, reads go through the first public endpoint that answers. One request at a time: some free
  // endpoints refuse batches.
  async function readOnlyProvider() {
    for (const url of S.net.rpcs || []) {
      const provider = new E.JsonRpcProvider(url, S.target, { staticNetwork: true, batchMaxCount: 1 });
      try {
        await Promise.race([provider.getBlockNumber(), sleep(6000).then(() => { throw new Error("timeout"); })]);
        return provider;
      } catch (e) {
        provider.destroy();
      }
    }
    return null;
  }

  async function setup() {
    const chip = $("networkChip");
    chip.textContent = S.net.name;

    let walletChain = null;
    if (window.ethereum) {
      try {
        const accounts = await window.ethereum.request({ method: "eth_accounts" });
        walletChain = Number(await window.ethereum.request({ method: "eth_chainId" }));
        if (accounts && accounts.length) { S.account = E.getAddress(accounts[0]); }
      } catch (e) { console.warn(e); }
      if (window.ethereum.on) {
        window.ethereum.on("accountsChanged", () => location.reload());
        window.ethereum.on("chainChanged", () => location.reload());
      }
    }
    S.onTarget = !!(S.account && walletChain === S.target);
    chip.classList.toggle("off", !!S.account && !S.onTarget);

    S.dep = await loadDeployment(S.target);
    applyStatic();
    renderConnect();
    if (!S.dep) {
      chip.classList.add("off");
      showNotice(t("noDeployment"), true);
      return;
    }
    if (S.net.explorer) {
      const link = $("contractLink");
      link.href = `${S.net.explorer}/address/${S.dep.mengerToken}`;
      link.hidden = false;
    }
    await startReading();
  }

  // Connects the contracts to a provider and reads; retries every 20 s while no endpoint answers
  async function startReading() {
    clearTimeout(S.retry);
    try {
      if (S.onTarget) {
        const provider = new E.BrowserProvider(window.ethereum);
        S.read = provider;
        S.signer = await provider.getSigner(S.account);
      } else {
        S.read = await readOnlyProvider();
        if (!S.read) { throw new Error("unreachable"); }
      }
      S.c = contracts(S.read);
      if (S.signer) { S.w = contracts(S.signer); }
      const [name, symbol] = await Promise.all([S.c.token.name(), S.c.token.symbol()]);
      S.tokenName = name;
      S.tokenSymbol = symbol;
      applyStatic();
      renderConnect();
      const ok = await refresh();
      if (!ok) { throw new Error("read failed"); }
      if (!S.timer) {
        S.timer = setInterval(() => { if (document.visibilityState === "visible" && !S.busy) { refresh(); } }, 20000);
      }
    } catch (e) {
      console.warn(e);
      S.c = null;
      showNotice(t("readError", { err: t("err.unreachable") }), true);
      S.retry = setTimeout(startReading, 20000);
    }
  }

  // Reads the whole state, one reading at a time; a reading asked during another runs right after it
  async function refresh() {
    if (S.refreshing) {
      S.refreshAgain = true;
      return S.refreshing;
    }
    let ok = false;
    S.refreshing = (async () => {
      try { ok = await readState(); } finally { S.refreshing = null; }
      return ok;
    })();
    await S.refreshing;
    if (S.refreshAgain) {
      S.refreshAgain = false;
      return refresh();
    }
    return ok;
  }

  async function readState() {
    if (!S.c) { return false; }
    try {
      const c = S.c;
      const price = await c.priceFeed.fetchPrice.staticCall();
      const [eth, xau, redOk, sysColl, sysDebt, tcr, recovery, count, borrowRate, redeemRate, deployTime, spTotal, supply, block] =
        await Promise.all([
          c.priceFeed.getEthUsdPrice(), c.priceFeed.getXauUsdPrice(), c.priceFeed.isRedemptionAllowed(),
          c.troveManager.getEntireSystemColl(), c.troveManager.getEntireSystemDebt(), c.troveManager.getTCR(price),
          c.troveManager.checkRecoveryMode(price), c.troveManager.getTroveOwnersCount(),
          c.troveManager.getBorrowingRateWithDecay(), c.troveManager.getRedemptionRateWithDecay(),
          c.troveManager.systemDeploymentTime(), c.stabilityPool.getTotalLUSDDeposits(), c.token.totalSupply(),
          S.read.getBlock("latest")
        ]);
      const D = {
        price, ethUsd: eth[0], ethReliable: eth[1], xauUsd: xau[0], goldAgreed: xau[1], goldFresh: xau[2], redOk,
        sysColl, sysDebt, tcr, recovery, count: Number(count), borrowRate, redeemRate, deployTime, spTotal, supply,
        now: BigInt(block.timestamp)
      };
      D.unitUsd = (D.xauUsd * UNIT_GRAMS) / OUNCE_GRAMS;
      D.weekend = await c.priceFeed.isGoldMarketClosedAt(D.now);

      D.risky = [];
      if (D.count > 0) {
        const list = await c.multiTroveGetter.getMultipleSortedTroves(-1, 8);
        D.risky = await Promise.all(list.map(async (row) => {
          const owner = row[0];
          const [icr, dc] = await Promise.all([c.troveManager.getCurrentICR(owner, price), c.troveManager.getEntireDebtAndColl(owner)]);
          return { owner, icr, debt: dc[0], coll: dc[1] };
        }));
      }
      D.liquidatable = liquidatableSet(D);
      // Stability Pool withdrawals are blocked while the lowest loan is below 110 %
      D.lowestUnderMcr = D.risky.length > 0 && D.risky[0].icr < MCR;

      if (S.account) {
        const a = S.account;
        const [ethBal, tokBal, status, dc, deposit, ethGain, feeGain, surplus] = await Promise.all([
          S.read.getBalance(a), c.token.balanceOf(a), c.troveManager.getTroveStatus(a), c.troveManager.getEntireDebtAndColl(a),
          c.stabilityPool.getCompoundedLUSDDeposit(a), c.stabilityPool.getDepositorETHGain(a),
          c.stabilityPool.getDepositorLQTYGain(a), c.collSurplusPool.getCollateral(a)
        ]);
        D.me = { ethBal, tokBal, active: Number(status) === 1, debt: dc[0], coll: dc[1], deposit, ethGain, feeGain, surplus };
        D.me.icr = D.me.active && D.me.debt > 0n ? (D.me.coll * price) / D.me.debt : 0n;
      }

      if (S.dep.testnetFeeds) {
        const [rounds, owner] = await Promise.all([Promise.all(c.feeds.map((f) => f.latestRoundData())), c.feeds[0].owner()]);
        D.feedAges = rounds.map((r) => (D.now > BigInt(r[3]) ? D.now - BigInt(r[3]) : 0n));
        D.feedAnswers = rounds.map((r) => BigInt(r[1]));
        D.feedOwner = owner;
      }

      S.data = D;
      showNotice("");
      render();
      return true;
    } catch (e) {
      console.error(e);
      showNotice(t("readError", { err: (e && (e.shortMessage || e.message)) || String(e) }), true);
      return false;
    }
  }

  // Which of the listed loans liquidateTroves() would liquidate now. Normal mode: below 110 %. Recovery mode: below
  // 110 %, or below the total ratio if the pool can absorb the whole debt. The last loan of the system never.
  function liquidatableSet(D) {
    const set = new Set();
    if (D.count <= 1) { return set; }
    const highest = D.count <= D.risky.length ? D.risky[D.risky.length - 1].owner : null;
    let room = D.spTotal > MIN_IN_SP ? D.spTotal - MIN_IN_SP : 0n;
    for (const r of D.risky) {
      if (r.owner === highest) { break; }
      if (r.icr < MCR) {
        set.add(r.owner);
        room -= r.debt < room ? r.debt : room;
        continue;
      }
      if (!D.recovery || room === 0n) { break; }
      if (r.icr < D.tcr && r.debt <= room) {
        set.add(r.owner);
        room -= r.debt;
      }
    }
    return set;
  }

  // --- Rendering ---

  function renderConnect() {
    if (!(S.account && S.onTarget)) {
      $("accountHint").textContent = S.account ? t("account.wrongNetwork") : (window.ethereum ? t("account.connectFirst") : t("noWallet"));
    }
    const btn = $("connectBtn");
    if (S.account && !S.onTarget) { btn.textContent = t("switch"); }
    else if (S.account) { btn.textContent = short(S.account); }
    else { btn.textContent = t("connect"); }
  }

  function render() {
    applyStatic();
    renderConnect();
    const D = S.data;
    if (!D) { return; }
    const act = S.onTarget && !S.busy;

    $("heroToday").textContent = t("hero.today", { usd: fmtUsd(toN(D.unitUsd)), price: num(toN(D.price), 2) });

    // System
    $("sysEthUsd").textContent = fmtUsd(toN(D.ethUsd)) + (D.ethReliable ? "" : ` (${t("system.unconfirmed")})`);
    $("sysXauUsd").textContent = fmtUsd(toN(D.xauUsd)) + (D.goldAgreed && D.goldFresh ? "" : ` (${t("system.unconfirmed")})`);
    $("sysUnitUsd").textContent = fmtUsd(toN(D.unitUsd));
    $("sysColl").textContent = fmtEth(D.sysColl, 3);
    $("sysDebt").textContent = fmtTok(D.sysDebt);
    const tcrEl = $("sysTcr");
    tcrEl.textContent = D.sysDebt > 0n ? fmtPct(D.tcr) : "–";
    tcrEl.className = D.sysDebt > 0n ? ratioClass(D.tcr) : "";
    $("sysMode").textContent = D.recovery ? t("system.mode.recovery") : t("system.mode.normal");
    $("sysMode").className = D.recovery ? "risk" : "";
    $("sysSp").textContent = fmtTok(D.spTotal);
    $("sysTroves").textContent = String(D.count);
    $("sysBorrowRate").textContent = fmtPct(D.recovery ? 0n : D.borrowRate, 2);
    $("sysRedeemRate").textContent = fmtPct(D.spTotal === 0n ? 0n : D.redeemRate, 2);

    // Riskiest loans
    const body = $("riskyBody");
    body.textContent = "";
    D.risky.forEach((r) => {
      const tr = document.createElement("tr");
      const th = document.createElement("th");
      th.scope = "row";
      th.textContent = short(r.owner);
      if (S.account && r.owner.toLowerCase() === S.account.toLowerCase()) {
        const tag = document.createElement("span");
        tag.className = "tag";
        tag.textContent = t("risky.you");
        th.append(tag);
      }
      const tdRatio = document.createElement("td");
      tdRatio.textContent = fmtPct(r.icr);
      tdRatio.className = D.liquidatable.has(r.owner) ? "bad" : ratioClass(r.icr);
      const tdDebt = document.createElement("td");
      tdDebt.textContent = fmtTok(r.debt);
      tr.append(th, tdRatio, tdDebt);
      body.append(tr);
    });
    $("riskyTable").hidden = D.risky.length === 0;
    $("riskyEmpty").hidden = D.risky.length !== 0;
    const n = D.liquidatable.size;
    const liq = $("liqBtn");
    liq.textContent = n === 0 ? t("risky.noLiq") : t(D.recovery ? "risky.liqBtnRecovery" : "risky.liqBtn", { n });
    liq.disabled = !act || n === 0;
    const note = $("riskyNote");
    if (D.count === 1 && D.lowestUnderMcr) { note.textContent = t("risky.lastNote"); note.hidden = false; }
    else if (D.recovery) { note.textContent = t("risky.recoveryNote", { tcr: fmtPct(D.tcr) }); note.hidden = false; }
    else { note.hidden = true; }

    renderAccount(D, act);
    renderDemo(D, act);
  }

  function renderAccount(D, act) {
    const connected = !!(S.account && S.onTarget && D.me);
    $("accountDisconnected").hidden = connected;
    $("accountConnected").hidden = !connected;
    if (!connected) {
      $("accountHint").textContent = S.account ? t("account.wrongNetwork") : (window.ethereum ? t("account.connectFirst") : t("noWallet"));
      return;
    }
    const me = D.me;

    $("balEth").textContent = fmtEth(me.ethBal);
    $("balTok").textContent = fmtTok(me.tokBal);
    $("faucetRow").hidden = !(S.net.faucets && me.ethBal < 10n ** 16n);

    $("surplusBox").hidden = me.surplus === 0n;
    if (me.surplus > 0n) { $("surplusText").textContent = t("surplus.text", { eth: num(toN(me.surplus), 4) }); }
    $("surplusBtn").disabled = !act;

    // Tabs
    ["borrow", "pool", "redeem"].forEach((name) => {
      const tab = $("tab-" + name);
      tab.setAttribute("aria-selected", String(S.tab === name));
      tab.tabIndex = S.tab === name ? 0 : -1;
      $("pane-" + name).hidden = S.tab !== name;
    });

    // Borrow
    $("openForm").hidden = me.active;
    $("troveView").hidden = !me.active;
    $("openBtn").disabled = !act;
    $("openFeeLabel").textContent = t("open.fee", { rate: fmtPct(D.recovery ? 0n : D.borrowRate, 2) });
    $("openReserve").textContent = fmtTok(GAS_COMP, 1);
    $("openRecovery").hidden = !D.recovery;
    previewOpen();
    if (me.active) {
      $("troveDebt").textContent = fmtTok(me.debt);
      $("troveColl").textContent = fmtEth(me.coll);
      const r = $("troveRatio");
      r.textContent = fmtPct(me.icr);
      r.className = ratioClass(me.icr);
      $("troveLiq").textContent = fmtUsd(liqUsd(me.coll, me.debt, D));
      const pct = Math.max(0, Math.min(1, (toN(me.icr) - 1) / 2)); // 100 % -> 0, 300 % -> 1
      $("meterMark").style.left = `${(pct * 100).toFixed(2)}%`;
      const atRisk = D.recovery && me.icr < D.tcr;
      $("troveRecovery").hidden = !atRisk;
      if (atRisk) { $("troveRecovery").textContent = t("trove.recoveryRisk", { tcr: fmtPct(D.tcr) }); }

      const repay = me.debt - GAS_COMP;
      $("closeText").textContent = t("close.text", { repay: num(toN(repay), 2), coll: num(toN(me.coll), 4) });
      const missing = repay > me.tokBal ? repay - me.tokBal : 0n;
      let blocked = "";
      if (D.count <= 1) { blocked = t("close.last"); }
      else if (D.recovery) { blocked = t("close.recovery"); }
      else if (missing > 0n) { blocked = t("close.missing", { bal: num(toN(me.tokBal), 2), missing: num(toN(missing), 2) }); }
      $("closeBlocked").textContent = blocked;
      $("closeBlocked").hidden = !blocked;
      $("closeBtn").disabled = !act || !!blocked;
      previewAdjust();
    }

    // Stability Pool
    $("poolDeposit").textContent = fmtTok(me.deposit);
    $("poolEthGain").textContent = fmtEth(me.ethGain, 6);
    $("poolFeeGain").textContent = fmtTok(me.feeGain, 4);
    const maxWithdraw = maxPoolWithdraw(D);
    $("poolMaxDeposit").textContent = t("pool.maxDeposit", { x: fmtTok(me.tokBal, 4) });
    $("poolMaxDeposit").hidden = me.tokBal === 0n;
    $("poolMaxWithdraw").textContent = t("pool.maxWithdraw", { x: fmtTok(maxWithdraw, 4) });
    $("poolMaxWithdraw").hidden = maxWithdraw === 0n;
    const help = $("poolHelp");
    if (D.lowestUnderMcr && me.deposit > 0n) {
      help.textContent = D.count === 1 ? t("pool.blockedLast") : t("pool.blocked");
      help.hidden = false;
    } else if (me.deposit > 0n && maxWithdraw < me.deposit) {
      help.textContent = t("pool.minLeft", { max: fmtTok(maxWithdraw, 4) });
      help.hidden = false;
    } else {
      help.hidden = true;
    }
    $("poolMinWarn").hidden = D.spTotal >= 2n * WAD;
    $("poolMinWarn").textContent = t("pool.minWarn");
    $("poolDepositBtn").disabled = !act || me.tokBal === 0n;
    $("poolWithdrawBtn").disabled = !act || maxWithdraw === 0n || D.lowestUnderMcr;
    $("poolClaimBtn").disabled = !act || (me.deposit === 0n && me.ethGain === 0n && me.feeGain === 0n);
    $("poolToTroveBtn").disabled = !act || !me.active || me.ethGain === 0n;

    // Redeem
    const reason = redemptionBlocker(D);
    const state = $("redeemState");
    state.textContent = reason ? t("redeem.closed", { reason }) : t("redeem.open");
    state.className = "state " + (reason ? "closed" : "open");
    $("redeemBtn").disabled = !act || !!reason || me.tokBal === 0n;
    previewRedeem();
  }

  function renderDemo(D, act) {
    const isOwner = !!(S.dep && S.dep.testnetFeeds && S.account && D.feedOwner && S.onTarget &&
      D.feedOwner.toLowerCase() === S.account.toLowerCase());
    $("demoPanel").hidden = !isOwner;
    if (!isOwner) { return; }
    if (!S.demoPrefilled) {
      $("demoEth").value = inputValue(D.feedAnswers[0], 8);
      $("demoXau").value = inputValue(D.feedAnswers[2], 8);
      S.demoPrefilled = true;
    }
    const keys = ["demo.feed.ethA", "demo.feed.ethB", "demo.feed.xauA", "demo.feed.xauB"];
    const tbody = $("demoAges");
    tbody.textContent = "";
    D.feedAges.forEach((age, i) => {
      const tr = document.createElement("tr");
      const th = document.createElement("th");
      th.scope = "row";
      th.textContent = t(keys[i]);
      const td = document.createElement("td");
      td.textContent = `${fmtUsd(toN(D.feedAnswers[i], 8))}, ${t("demo.ago", { t: fmtDuration(age) })}`;
      tr.append(th, td);
      tbody.append(tr);
    });
    ["demoPublish", "demoRefresh", "demoCrash", "demoGoldUp"].forEach((id) => { $(id).disabled = !act; });
  }

  // USD price of ETH at which a trove reaches 110 %, with the gold price unchanged
  function liqUsd(coll, debt, D) {
    if (coll === 0n) { return 0; }
    return toN((MCR * debt) / coll) * toN(D.unitUsd);
  }

  function maxPoolWithdraw(D) {
    if (!D.me) { return 0n; }
    const room = D.spTotal > MIN_IN_SP ? D.spTotal - MIN_IN_SP : 0n;
    return D.me.deposit < room ? D.me.deposit : room;
  }

  function redemptionBlocker(D) {
    if (D.weekend) { return t("redeem.reason.weekend"); }
    const opens = D.deployTime + BOOTSTRAP;
    if (D.now < opens) { return t("redeem.reason.bootstrap", { left: fmtDuration(opens - D.now) }); }
    if (!D.goldAgreed || !D.goldFresh) { return t("redeem.reason.gold"); }
    if (!D.ethReliable) { return t("redeem.reason.eth"); }
    if (D.sysDebt > 0n && D.tcr < MCR) { return t("redeem.reason.tcr"); }
    if (!D.redOk) { return t("redeem.reason.gold"); }
    return null;
  }

  // --- Previews ---

  function setPreview(id, text, cls) {
    const el = $(id);
    el.textContent = text;
    el.className = cls || "";
  }

  function previewOpen() {
    const D = S.data;
    if (!D) { return; }
    const coll = readAmount("openColl").value;
    const amount = readAmount("openAmount").value;
    if (!amount) {
      ["openFee", "openDebt", "openRatio", "openLiq"].forEach((id) => setPreview(id, "–"));
      return;
    }
    const fee = D.recovery ? 0n : (amount * D.borrowRate) / WAD;
    const debt = amount + fee + GAS_COMP;
    setPreview("openFee", fmtTok(fee, 4));
    setPreview("openDebt", fmtTok(debt));
    if (!coll) {
      setPreview("openRatio", "–");
      setPreview("openLiq", "–");
      return;
    }
    const icr = (coll * D.price) / debt;
    setPreview("openRatio", fmtPct(icr), D.recovery && icr < CCR ? "bad" : ratioClass(icr));
    setPreview("openLiq", fmtUsd(liqUsd(coll, debt, D)));
  }

  function adjustOp() {
    const checked = document.querySelector('input[name="adj"]:checked');
    return checked ? checked.value : "add";
  }

  function maxRepay(me) {
    const floor = MIN_NET_DEBT + GAS_COMP;
    return me.debt > floor ? me.debt - floor : 0n;
  }

  function previewAdjust() {
    const D = S.data;
    if (!D || !D.me || !D.me.active) { return; }
    const op = adjustOp();
    const me = D.me;
    $("adjUnit").textContent = op === "add" || op === "withdraw" ? "ETH" : S.tokenSymbol;
    $("adjBtn").textContent = t("adjust." + op);
    const x = readAmount("adjAmount").value;
    let coll = me.coll;
    let debt = me.debt;
    let fee = 0n;
    if (x) {
      if (op === "add") { coll += x; }
      if (op === "withdraw") { coll = coll > x ? coll - x : 0n; }
      if (op === "borrow") { fee = D.recovery ? 0n : (x * D.borrowRate) / WAD; debt += x + fee; }
      if (op === "repay") { debt = debt > x ? debt - x : 0n; }
    }
    $("adjFeeRow").hidden = op !== "borrow";
    $("adjFee").textContent = fmtTok(fee, 4);
    const help = $("adjHelp");
    if (op === "repay") {
      help.textContent = t("adjust.maxRepay", { max: num(toN(maxRepay(me)), 2) });
      help.hidden = false;
    } else {
      help.hidden = true;
    }
    if (!x) {
      setPreview("adjDebt", "–");
      setPreview("adjRatio", "–");
    } else {
      setPreview("adjDebt", fmtTok(debt));
      const icr = debt > 0n ? (coll * D.price) / debt : 0n;
      setPreview("adjRatio", debt > 0n ? fmtPct(icr) : "–", debt > 0n ? ratioClass(icr) : "");
    }
    $("adjBtn").disabled = !S.onTarget || S.busy;
  }

  function redemptionQuote(amount) {
    const D = S.data;
    const ethDrawn = (amount * WAD) / D.price;
    // The base rate rises by amount / supply / BETA (2); no fee at all while the Stability Pool is empty
    let rate = D.redeemRate + (D.supply > 0n ? (amount * WAD) / D.supply / 2n : 0n);
    if (rate > WAD) { rate = WAD; }
    if (D.spTotal === 0n) { rate = 0n; }
    const fee = (ethDrawn * rate) / WAD;
    return { ethDrawn, rate, fee, receive: ethDrawn - fee };
  }

  function previewRedeem() {
    const D = S.data;
    if (!D) { return; }
    const amount = readAmount("redeemAmount").value;
    const note = $("redeemTruncated");
    if (!amount) {
      setPreview("redeemFee", fmtPct(D.spTotal === 0n ? 0n : D.redeemRate, 2));
      setPreview("redeemReceive", "–");
      note.hidden = true;
      return;
    }
    const check = S.redeemCheck;
    const known = check.amount !== null && check.amount === amount ? check.truncated : null;
    const q = redemptionQuote(known !== null && known > 0n ? known : amount);
    setPreview("redeemFee", `${fmtPct(q.rate, 2)}, ${fmtEth(q.fee, 6)}`);
    setPreview("redeemReceive", fmtEth(q.receive, 6));
    if (known !== null && known < amount) {
      note.textContent = known === 0n ? t("err.nothingRedeem") : t("redeem.truncatedPreview", { amount: num(toN(known), 4) });
      note.hidden = false;
    } else {
      note.hidden = true;
    }
  }

  // Asks HintHelpers how much of the amount can be redeemed in one transaction, before the visitor signs
  function scheduleRedeemCheck() {
    clearTimeout(S.redeemTimer);
    previewRedeem();
    const amount = readAmount("redeemAmount").value;
    if (!amount || !S.c || !S.data) { return; }
    S.redeemTimer = setTimeout(async () => {
      const seq = ++S.redeemCheck.seq;
      try {
        const h = await S.c.hintHelpers.getRedemptionHints(amount, S.data.price, 0n);
        if (seq !== S.redeemCheck.seq) { return; }
        S.redeemCheck.amount = amount;
        S.redeemCheck.truncated = h[2];
        previewRedeem();
      } catch (e) { console.warn(e); }
    }, 300);
  }

  // --- Actions ---

  async function connect() {
    if (!window.ethereum) {
      showNotice(t("noWallet"));
      return;
    }
    try {
      if (S.account && !S.onTarget) {
        await switchNetwork();
        return;
      }
      await window.ethereum.request({ method: "eth_requestAccounts" });
      const chain = Number(await window.ethereum.request({ method: "eth_chainId" }));
      if (chain !== S.target) { await switchNetwork(); }
      location.reload();
    } catch (e) {
      console.error(e);
      showNotice(explain(e), true);
    }
  }

  async function switchNetwork() {
    try {
      await window.ethereum.request({ method: "wallet_switchEthereumChain", params: [{ chainId: S.net.hex }] });
    } catch (e) {
      const unknownChain = e && (e.code === 4902 || (e.data && e.data.originalError && e.data.originalError.code === 4902));
      if (unknownChain && S.net.add) {
        await window.ethereum.request({ method: "wallet_addEthereumChain", params: [S.net.add] });
      } else {
        throw e;
      }
    }
  }

  async function openTrove(ev) {
    ev.preventDefault();
    const status = $("openStatus");
    const D = S.data;
    const c = readAmount("openColl");
    const a = readAmount("openAmount");
    if (c.error || a.error) { setStatus(status, "error", c.error || a.error); return; }
    const coll = c.value;
    const amount = a.value;
    if (amount < MIN_NET_DEBT) { setStatus(status, "error", t("err.minDebt")); return; }
    if (coll > D.me.ethBal) { setStatus(status, "error", t("err.balance")); return; }
    const fee = D.recovery ? 0n : (amount * D.borrowRate) / WAD;
    const maxFee = maxBorrowFee();
    await runTxs(status, [async () => {
      const [up, low] = await insertHints(nicr(coll, amount + fee + GAS_COMP));
      return send(S.w.borrowerOperations, "openTrove", [maxFee, amount, up, low], { value: coll });
    }], "open.done", {
      doneEl: $("troveStatus"),
      clear: () => { $("openColl").value = ""; $("openAmount").value = ""; }
    });
  }

  async function adjustTrove(ev) {
    ev.preventDefault();
    const status = $("adjStatus");
    const D = S.data;
    const me = D.me;
    const op = adjustOp();
    const r = readAmount("adjAmount");
    if (r.error) { setStatus(status, "error", r.error); return; }
    const x = r.value;
    if (op === "add" && x > me.ethBal) { setStatus(status, "error", t("err.balance")); return; }
    if (op === "withdraw" && x >= me.coll) { setStatus(status, "error", t("err.tooMuchColl", { coll: num(toN(me.coll), 4) })); return; }
    if (op === "repay" && x > maxRepay(me)) { setStatus(status, "error", t("err.tooMuchRepay", { max: num(toN(maxRepay(me)), 2) })); return; }
    if (op === "repay" && x > me.tokBal) { setStatus(status, "error", t("err.balance")); return; }
    let coll = me.coll;
    let debt = me.debt;
    let collWithdrawal = 0n;
    let debtChange = 0n;
    let increase = false;
    let value = 0n;
    if (op === "add") { value = x; coll += x; }
    if (op === "withdraw") { collWithdrawal = x; coll -= x; }
    if (op === "borrow") {
      debtChange = x;
      increase = true;
      debt += x + (D.recovery ? 0n : (x * D.borrowRate) / WAD);
    }
    if (op === "repay") { debtChange = x; debt -= x; }
    const maxFee = maxBorrowFee();
    await runTxs(status, [async () => {
      const [up, low] = await insertHints(nicr(coll, debt));
      return send(S.w.borrowerOperations, "adjustTrove", [maxFee, collWithdrawal, debtChange, increase, up, low], { value });
    }], "adjust.done", { clear: () => { $("adjAmount").value = ""; } });
  }

  async function closeTrove() {
    await runTxs($("closeStatus"), [() => send(S.w.borrowerOperations, "closeTrove", [])], "close.done", { doneEl: $("openStatus") });
  }

  async function claimSurplus() {
    await runTxs($("surplusStatus"), [() => send(S.w.borrowerOperations, "claimCollateral", [])], "surplus.done", { doneEl: $("accountStatus") });
  }

  async function poolDeposit(ev) {
    ev.preventDefault();
    const status = $("poolStatus");
    const r = readAmount("poolAmount");
    if (r.error) { setStatus(status, "error", r.error); return; }
    if (r.value > S.data.me.tokBal) { setStatus(status, "error", t("err.balance")); return; }
    await runTxs(status, [() => send(S.w.stabilityPool, "provideToSP", [r.value, ZERO])], "pool.done.deposit",
      { clear: () => { $("poolAmount").value = ""; } });
  }

  async function poolWithdraw() {
    const status = $("poolStatus");
    const r = readAmount("poolAmount");
    if (r.error) { setStatus(status, "error", r.error); return; }
    const max = maxPoolWithdraw(S.data);
    if (r.value > max) { setStatus(status, "error", t("pool.minLeft", { max: fmtTok(max, 4) })); return; }
    await runTxs(status, [() => send(S.w.stabilityPool, "withdrawFromSP", [r.value])], "pool.done.withdraw",
      { clear: () => { $("poolAmount").value = ""; } });
  }

  async function poolClaim() {
    await runTxs($("poolStatus"), [() => send(S.w.stabilityPool, "withdrawFromSP", [0n])], "pool.done.claim");
  }

  async function poolToTrove() {
    const me = S.data.me;
    await runTxs($("poolStatus"), [async () => {
      const [up, low] = await insertHints(nicr(me.coll + me.ethGain, me.debt));
      return send(S.w.stabilityPool, "withdrawETHGainToTrove", [up, low]);
    }], "pool.done.toTrove");
  }

  // The amounts actually redeemed, from the TroveManager's Redemption event
  function redemptionResult(receipts) {
    const iface = S.c.troveManager.interface;
    for (const receipt of receipts) {
      for (const log of receipt.logs) {
        if (log.address.toLowerCase() !== S.dep.troveManager.toLowerCase()) { continue; }
        try {
          const ev = iface.parseLog(log);
          if (ev && ev.name === "Redemption") {
            return t("redeem.doneDetail", { amount: num(toN(ev.args[1]), 4), eth: num(toN(ev.args[2] - ev.args[3]), 6) });
          }
        } catch (e) { /* another event */ }
      }
    }
    return t("redeem.done");
  }

  async function redeem(ev) {
    ev.preventDefault();
    const status = $("redeemStatus");
    const D = S.data;
    const r = readAmount("redeemAmount");
    if (r.error) { setStatus(status, "error", r.error); return; }
    const amount = r.value;
    if (amount > D.me.tokBal) { setStatus(status, "error", t("err.balance")); return; }
    await runTxs(status, [async () => {
      const price = await S.c.priceFeed.fetchPrice.staticCall();
      const h = await S.c.hintHelpers.getRedemptionHints(amount, price, 0n);
      const first = h[0];
      const partialNICR = h[1];
      const redeemed = h[2];
      if (redeemed === 0n) { throw localError(t("err.nothingRedeem")); }
      const [up, low] = partialNICR > 0n ? await insertHints(partialNICR) : [ZERO, ZERO];
      let maxFee = redemptionQuote(redeemed).rate + SLIPPAGE;
      if (maxFee < FEE_FLOOR) { maxFee = FEE_FLOOR; }
      if (maxFee > WAD) { maxFee = WAD; }
      return send(S.w.troveManager, "redeemCollateral", [redeemed, first, up, low, partialNICR, 0n, maxFee]);
    }], redemptionResult, { clear: () => { $("redeemAmount").value = ""; S.redeemCheck.amount = null; } });
  }

  async function liquidate() {
    await runTxs($("liqStatus"), [() => send(S.w.troveManager, "liquidateTroves", [10n])], "risky.done");
  }

  // Rounds of at most -40 % or +60 % from a feed's own current answer to the target, as script/SetTestnetPrices.s.sol
  function priceSteps(current, targetAnswer) {
    const steps = [];
    let cur = current;
    while (cur !== targetAnswer) {
      let next = targetAnswer;
      if (targetAnswer * 10n < cur * 6n) { next = (cur * 6n) / 10n; }
      else if (targetAnswer * 10n > cur * 16n) { next = (cur * 16n) / 10n; }
      steps.push(next);
      cur = next;
    }
    return steps;
  }

  async function publishPrices(ev) {
    ev.preventDefault();
    const status = $("demoStatus");
    const eth = readNumber($("demoEth").value, 8);
    const xau = readNumber($("demoXau").value, 8);
    if (eth.error || xau.error) { setStatus(status, "error", eth.error || xau.error); return; }
    const answers = await Promise.all(S.c.feeds.map((f) => f.answer()));
    const targets = [eth.value, eth.value, xau.value, xau.value];
    // Each feed moves from its own answer, so a sequence interrupted halfway is completed by publishing again
    const perFeed = targets.map((tv, i) => priceSteps(BigInt(answers[i]), tv));
    const txs = [];
    [[0, 1], [2, 3]].forEach(([a, b]) => {
      const rounds = Math.max(perFeed[a].length, perFeed[b].length);
      for (let k = 0; k < rounds; k++) {
        if (k < perFeed[a].length) { const p = perFeed[a][k]; txs.push(() => send(S.w.feeds[a], "setAnswer", [p])); }
        if (k < perFeed[b].length) { const p = perFeed[b][k]; txs.push(() => send(S.w.feeds[b], "setAnswer", [p])); }
      }
    });
    if (txs.length === 0) { setStatus(status, "done", t("demo.nothing")); return; }
    txs.push(() => send(S.w.priceFeed, "fetchPrice", []));
    if (txs.length > MANY_STEPS && !window.confirm(t("demo.many", { n: txs.length }))) { return; }
    await runTxs(status, txs, "demo.done");
  }

  async function refreshFeeds() {
    const txs = S.w.feeds.map((f) => () => send(f, "refresh", []));
    txs.push(() => send(S.w.priceFeed, "fetchPrice", []));
    await runTxs($("demoStatus"), txs, "demo.refreshed");
  }

  function presetPrice(id, factor) {
    const r = readNumber($(id).value, 8);
    if (r.error) { return; }
    const v = (r.value * BigInt(Math.round(factor * 1000))) / 1000n;
    $(id).value = inputValue((v / 1000000n) * 1000000n, 8); // whole cents
  }

  async function watchToken() {
    try {
      await window.ethereum.request({
        method: "wallet_watchAsset",
        params: { type: "ERC20", options: { address: S.dep.mengerToken, symbol: S.tokenSymbol, decimals: 18 } }
      });
    } catch (e) { console.warn(e); }
  }

  // Rewrites the numbers already typed in a field when the language (and so the decimal separator) changes
  function relocalizeInputs(fromLang) {
    const fields = [["openColl", 18], ["openAmount", 18], ["adjAmount", 18], ["poolAmount", 18], ["redeemAmount", 18], ["demoEth", 8], ["demoXau", 8]];
    fields.forEach(([id, dec]) => {
      const el = $(id);
      if (!el.value) { return; }
      const r = readNumber(el.value, dec, fromLang);
      if (r.value) { el.value = inputValue(r.value, dec); }
    });
  }

  // --- Events ---

  function bind() {
    $("connectBtn").addEventListener("click", connect);
    document.querySelectorAll(".lang button").forEach((b) => b.addEventListener("click", () => {
      const from = S.lang;
      S.lang = b.dataset.lang;
      saveLang(S.lang);
      relocalizeInputs(from);
      if (S.data) { render(); } else { applyStatic(); renderConnect(); }
    }));

    const tabs = ["borrow", "pool", "redeem"];
    tabs.forEach((name, i) => {
      const tab = $("tab-" + name);
      tab.addEventListener("click", () => { S.tab = name; render(); });
      tab.addEventListener("keydown", (e) => {
        if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") { return; }
        const next = tabs[(i + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
        S.tab = next;
        render();
        $("tab-" + next).focus();
      });
    });

    $("openForm").addEventListener("submit", openTrove);
    ["openColl", "openAmount"].forEach((id) => $(id).addEventListener("input", previewOpen));
    $("adjustForm").addEventListener("submit", adjustTrove);
    $("adjAmount").addEventListener("input", previewAdjust);
    document.querySelectorAll('input[name="adj"]').forEach((r) => r.addEventListener("change", () => {
      setStatus($("adjStatus"), "", "");
      previewAdjust();
    }));
    $("closeBtn").addEventListener("click", closeTrove);
    $("surplusBtn").addEventListener("click", claimSurplus);

    $("poolForm").addEventListener("submit", poolDeposit);
    $("poolWithdrawBtn").addEventListener("click", poolWithdraw);
    $("poolClaimBtn").addEventListener("click", poolClaim);
    $("poolToTroveBtn").addEventListener("click", poolToTrove);
    $("poolMaxDeposit").addEventListener("click", () => { if (S.data && S.data.me) { $("poolAmount").value = inputValue(S.data.me.tokBal); } });
    $("poolMaxWithdraw").addEventListener("click", () => { if (S.data && S.data.me) { $("poolAmount").value = inputValue(maxPoolWithdraw(S.data)); } });

    $("redeemForm").addEventListener("submit", redeem);
    $("redeemAmount").addEventListener("input", scheduleRedeemCheck);
    $("liqBtn").addEventListener("click", liquidate);

    $("demoForm").addEventListener("submit", publishPrices);
    $("demoRefresh").addEventListener("click", refreshFeeds);
    $("demoCrash").addEventListener("click", () => presetPrice("demoEth", 0.7));
    $("demoGoldUp").addEventListener("click", () => presetPrice("demoXau", 1.03));
    $("watchBtn").addEventListener("click", watchToken);
  }

  S.lang = loadLang();
  applyStatic();
  renderConnect();
  showNotice(t("loading"));
  bind();
  setup().catch((e) => {
    console.error(e);
    showNotice(t("readError", { err: t("err.unreachable") }), true);
  });
})();
