//! A private Bitcoin network on this computer (regtest), for tests and demonstrations.
//! Blocks are mined on demand; nothing touches real bitcoin.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Bitcoind {
    child: Child,
    pub datadir: PathBuf,
    pub rpc_port: u16,
    pub user: String,
    pub password: String,
}

/// A free TCP port on localhost.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).expect("no free port")
}

impl Bitcoind {
    /// Starts `bitcoind -regtest` with its data in `datadir` and waits until it answers.
    pub fn start(exe: &Path, datadir: &Path) -> Result<Bitcoind> {
        std::fs::create_dir_all(datadir)?;
        let rpc_port = free_port();
        let p2p_port = free_port();
        let child = Command::new(exe)
            .args([
                "-regtest".to_string(),
                format!("-datadir={}", datadir.display()),
                format!("-rpcport={rpc_port}"),
                format!("-port={p2p_port}"),
                "-listen=0".into(),
                "-server=1".into(),
                "-txindex=1".into(),
                "-rest=1".into(),
                "-fallbackfee=0.0002".into(),
                "-printtoconsole=0".into(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {}", exe.display()))?;
        let cookie = datadir.join("regtest").join(".cookie");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(text) = std::fs::read_to_string(&cookie) {
                if let Some((user, password)) = text.trim().split_once(':') {
                    let mut b = Bitcoind {
                        child,
                        datadir: datadir.to_path_buf(),
                        rpc_port,
                        user: user.into(),
                        password: password.into(),
                    };
                    let ready = Instant::now() + Duration::from_secs(60);
                    while Instant::now() < ready {
                        if b.rpc("getblockchaininfo", json!([])).is_ok() {
                            return Ok(b);
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    let _ = b.child.kill();
                    bail!("bitcoind does not answer");
                }
            }
            if Instant::now() > deadline {
                bail!("bitcoind did not start");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}/", self.rpc_port);
        let auth = format!("{}:{}", self.user, self.password);
        let header = format!("Basic {}", base64(auth.as_bytes()));
        let body = json!({"jsonrpc": "1.0", "id": "menger", "method": method, "params": params});
        let resp = ureq::post(&url).set("Authorization", &header).timeout(Duration::from_secs(30)).send_json(body);
        let v: Value = match resp {
            Ok(r) => r.into_json()?,
            Err(ureq::Error::Status(_, r)) => r.into_json()?,
            Err(e) => return Err(anyhow!("rpc {method}: {e}")),
        };
        if !v["error"].is_null() {
            bail!("rpc {method}: {}", v["error"]);
        }
        Ok(v["result"].clone())
    }

    pub fn mine(&self, blocks: u64, address: &str) -> Result<()> {
        self.rpc("generatetoaddress", json!([blocks, address]))?;
        Ok(())
    }

    pub fn height(&self) -> Result<u64> {
        self.rpc("getblockcount", json!([]))?.as_u64().ok_or_else(|| anyhow!("bad block count"))
    }

    pub fn mempool_size(&self) -> Result<usize> {
        Ok(self.rpc("getrawmempool", json!([]))?.as_array().map(Vec::len).unwrap_or(0))
    }
}

impl Drop for Bitcoind {
    fn drop(&mut self) {
        let _ = self.rpc("stop", json!([]));
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
    }
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_reference() {
        assert_eq!(super::base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(super::base64(b"a"), "YQ==");
        assert_eq!(super::base64(b"ab"), "YWI=");
    }
}
