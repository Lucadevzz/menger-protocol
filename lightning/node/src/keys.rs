//! The wallet's secret: a 12-word recovery phrase (BIP 39), kept in the data folder.
//!
//! The phrase brings back the bitcoin of the on-chain wallet. The money in channels also needs
//! the rest of the data folder, which records the state of each channel.

use anyhow::{anyhow, bail, Context, Result};
use ldk_node::bip39::{Mnemonic, WordCount};
use ldk_node::entropy::NodeEntropy;
use std::path::Path;

/// File, in the data folder, that holds the recovery phrase.
pub const PHRASE_FILE: &str = "phrase-de-recuperation.txt";

pub struct Keys {
    pub entropy: NodeEntropy,
    /// The phrase, when it was just created: to be shown once to its owner.
    pub new_phrase: Option<String>,
    /// The wallet was restored from a phrase given by its owner.
    pub restored: bool,
}

/// The node's secret, from the first of:
/// 1. the seed file of a node created with LDK Node 0.7, before phrases;
/// 2. the phrase file;
/// 3. the phrase given to restore a wallet, in a folder that has none;
/// 4. a new phrase, written to the phrase file.
pub fn load_or_create(data_dir: &Path, restore: Option<&str>) -> Result<Keys> {
    let legacy_seed = data_dir.join("ldk").join("keys_seed");
    let phrase_path = data_dir.join(PHRASE_FILE);
    if restore.is_some() && (legacy_seed.exists() || phrase_path.exists()) {
        bail!("ce dossier contient déjà un portefeuille : on restaure dans un dossier neuf");
    }
    if legacy_seed.exists() {
        let entropy = NodeEntropy::from_seed_path(legacy_seed.to_string_lossy().into_owned())
            .map_err(|e| anyhow!("reading {}: {e:?}", legacy_seed.display()))?;
        return Ok(Keys { entropy, new_phrase: None, restored: false });
    }
    if phrase_path.exists() {
        let text = std::fs::read_to_string(&phrase_path).with_context(|| format!("reading {}", phrase_path.display()))?;
        let mnemonic = parse(&text)?;
        return Ok(Keys { entropy: NodeEntropy::from_bip39_mnemonic(mnemonic, None), new_phrase: None, restored: false });
    }
    let (mnemonic, restored) = match restore {
        Some(words) => (parse(words)?, true),
        None => (Mnemonic::generate(WordCount::Words12).map_err(|e| anyhow!("creating a phrase: {e}"))?, false),
    };
    write_private(&phrase_path, &format!("{mnemonic}\n"))?;
    let new_phrase = (!restored).then(|| mnemonic.to_string());
    Ok(Keys { entropy: NodeEntropy::from_bip39_mnemonic(mnemonic, None), new_phrase, restored })
}

fn parse(text: &str) -> Result<Mnemonic> {
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    Mnemonic::parse(&words).map_err(|e| anyhow!("phrase de récupération invalide : {e}"))
}

/// Writes a new file that only its owner can read.
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn a_new_wallet_gets_a_phrase_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path(), None).unwrap();
        let phrase = first.new_phrase.expect("a new phrase");
        assert_eq!(phrase.split(' ').count(), 12);
        assert_eq!(std::fs::read_to_string(dir.path().join(PHRASE_FILE)).unwrap().trim(), phrase);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(PHRASE_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "nobody else can read it");
        }
        let again = load_or_create(dir.path(), None).unwrap();
        assert!(again.new_phrase.is_none() && !again.restored);
    }

    #[test]
    fn restoring_needs_an_empty_folder_and_a_valid_phrase() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_or_create(dir.path(), Some("not a phrase")).is_err());
        let keys = load_or_create(dir.path(), Some(&PHRASE.to_uppercase().replace(' ', "  "))).unwrap();
        assert!(keys.restored && keys.new_phrase.is_none());
        assert_eq!(std::fs::read_to_string(dir.path().join(PHRASE_FILE)).unwrap().trim(), PHRASE);
        assert!(load_or_create(dir.path(), Some(PHRASE)).is_err(), "never over an existing wallet");
    }

    #[test]
    fn nodes_created_before_phrases_keep_their_seed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ldk")).unwrap();
        std::fs::write(dir.path().join("ldk").join("keys_seed"), [7u8; 64]).unwrap();
        let keys = load_or_create(dir.path(), None).unwrap();
        assert!(keys.new_phrase.is_none());
        assert!(!dir.path().join(PHRASE_FILE).exists());
    }
}
