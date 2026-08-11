//! Which OS process backs which managed session — the fact that lets `/mcp`
//! authenticate a caller by PROVENANCE instead of by which config entry its
//! vendor happened to load.
//!
//! **Why this exists.** A managed session's per-session MCP credential is
//! handed to the vendor at spawn (ACP `session/new mcpServers[]`), but whether
//! the vendor USES it is the vendor's call: the ACP dialect has no
//! `--strict-mcp-config` analogue on any of the three vendors, and grok 1.0.0
//! was measured resolving a same-named `ccteam` collision in favour of its own
//! global config — so the session's tools authenticated as the machine-wide
//! enrollment credential, its children mounted under a ghost ledger node, and
//! its project scope was not its own (see [`crate::execution::mcp_config`]'s
//! module doc for the ruled-out vendor levers).
//!
//! The daemon, however, SPAWNED that process. Recording the child pid here —
//! before the first handshake byte — inverts the fight: the `/mcp` endpoint
//! resolves a loopback peer back to the owning managed session by walking
//! `/proc`, whichever credential the vendor presented. The vendor's config
//! resolution stops deciding identity, which is the only way to win an
//! argument no vendor gives ccteam a flag for.
//!
//! Process-global on purpose: "which child pid belongs to which sid" is a fact
//! about THIS daemon process, and the `/mcp` route that reads it lives in a
//! different crate (`ccteam-web`) than the adapters that know it. ACP sessions
//! are local-only (remote ACP is an explicit NotImplemented), so a recorded
//! pid is always resolvable against the local `/proc`.
//!
//! Same-uid honesty: within one OS user this is soft identity like every other
//! rung (a same-uid process can already read a sibling's env). What it fixes
//! is not an attack but a MISATTRIBUTION — the session's own tool calls
//! landing on the machine identity.

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

fn registry() -> &'static RwLock<BTreeMap<String, u32>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, u32>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Record the vendor process backing `sid`. Call BEFORE the first handshake
/// I/O: the vendor dials `/mcp` inside `session/new`, and a pid recorded after
/// that returns is recorded too late for the very `initialize` it exists to
/// identify (use [`crate::execution::acp::AcpTransport::spawn_for_session`],
/// which gets the ordering right by construction). Keyed by sid, so a
/// resume-fallback re-spawn simply replaces the dead process. `None` (child
/// already reaped) records nothing.
pub fn record(sid: &str, pid: Option<u32>) {
    let Some(pid) = pid else { return };
    if sid.is_empty() {
        return;
    }
    if let Ok(mut map) = registry().write() {
        map.insert(sid.to_string(), pid);
    }
}

/// Drop a session's record. A record left behind is harmless — a dead pid is
/// pruned on the next read, and pid reuse cannot mint authority because the
/// attach still requires the sid's LIVE principal
/// (`SessionPrincipals::credential_for_managed_attach`).
pub fn forget(sid: &str) {
    if let Ok(mut map) = registry().write() {
        map.remove(sid);
    }
}

/// Live `(sid, root pid)` pairs. Dead pids are pruned here rather than on a
/// timer: the one reader is the provenance resolver, so read time is exactly
/// when staleness would matter.
pub fn roots() -> Vec<(String, u32)> {
    let live: Vec<(String, u32)> = registry()
        .read()
        .map(|m| {
            m.iter()
                .filter(|(_, pid)| process_alive(**pid))
                .map(|(sid, pid)| (sid.clone(), *pid))
                .collect()
        })
        .unwrap_or_default();
    if let Ok(mut map) = registry().write() {
        map.retain(|_, pid| process_alive(*pid));
    }
    live
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// Resolve the managed session whose process subtree holds the client end of a
/// loopback TCP connection with source port `port`.
///
/// `None` means "no managed process owns this connection" — a hand-started
/// agent, a satellite, or a non-linux host (the resolver reads `/proc`); the
/// caller falls back to the enrollment ladder, which is exactly the
/// pre-provenance behaviour. The walk is bounded to the registered roots'
/// subtrees: a process outside them could never map to a sid anyway.
pub fn owner_of_local_peer(port: u16) -> Option<String> {
    let roots = roots();
    if roots.is_empty() {
        return None;
    }
    imp::owner_of_local_peer(port, &roots)
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn owner_of_local_peer(_port: u16, _roots: &[(String, u32)]) -> Option<String> {
        None
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{BTreeMap, BTreeSet};

    pub fn owner_of_local_peer(port: u16, roots: &[(String, u32)]) -> Option<String> {
        let inodes = client_socket_inodes(port);
        if inodes.is_empty() {
            return None;
        }
        let owners = subtree_owners(roots, &pid_table());
        owners
            .iter()
            .find(|(pid, _)| pid_holds_socket(*pid, &inodes))
            .map(|(_, sid)| sid.clone())
    }

    /// Socket inodes of ESTABLISHED loopback connections whose LOCAL port is
    /// `port` — i.e. the CLIENT end of the `/mcp` request being served (the
    /// server saw that port as its peer).
    fn client_socket_inodes(port: u16) -> Vec<u64> {
        let mut inodes = Vec::new();
        for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in content.lines().skip(1) {
                if let Some(inode) = parse_established_loopback_local(line, port) {
                    inodes.push(inode);
                }
            }
        }
        inodes
    }

    /// One `/proc/net/tcp{,6}` row → its socket inode, when the row is an
    /// ESTABLISHED (`st == 01`) connection whose LOCAL end is `port` on a
    /// loopback address. Row shape (header skipped by the caller):
    ///
    /// ```text
    /// sl local_address rem_address st tx_queue:rx_queue tr:tm->when retrnsmt uid timeout inode …
    /// ```
    ///
    /// Addresses are hex, ip little-endian per 32-bit group.
    pub(super) fn parse_established_loopback_local(line: &str, port: u16) -> Option<u64> {
        let mut fields = line.split_whitespace();
        let _sl = fields.next()?;
        let local = fields.next()?;
        let _rem = fields.next()?;
        if fields.next()? != "01" {
            return None;
        }
        let (ip_hex, port_hex) = local.rsplit_once(':')?;
        if u16::from_str_radix(port_hex, 16).ok()? != port {
            return None;
        }
        if !is_loopback_hex(ip_hex) {
            return None;
        }
        // Past st: tx:rx, tr:when, retrnsmt, uid, timeout, then inode.
        let inode = fields.nth(5)?;
        inode.parse::<u64>().ok().filter(|i| *i != 0)
    }

    /// Loopback in `/proc/net` hex: v4 `127.x.y.z` puts its first octet in the
    /// LOWEST byte of the little-endian u32, so the row ends `7F`; v6 is `::1`
    /// or a v4-mapped `::ffff:127.x.y.z` (same trailing group shape as v4).
    fn is_loopback_hex(ip_hex: &str) -> bool {
        match ip_hex.len() {
            8 => ip_hex.ends_with("7F"),
            32 => ip_hex == "00000000000000000000000001000000" || ip_hex.ends_with("7F"),
            _ => false,
        }
    }

    /// `(pid, ppid)` for every process `/proc` will show us.
    fn pid_table() -> Vec<(u32, u32)> {
        let mut table = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return table;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some(ppid) = parse_stat_ppid(&stat) {
                table.push((pid, ppid));
            }
        }
        table
    }

    /// `/proc/<pid>/stat` field 4 (ppid), parsed after the LAST `)` because
    /// field 2 (comm) may itself contain spaces and parentheses.
    pub(super) fn parse_stat_ppid(stat: &str) -> Option<u32> {
        let after = stat.rsplit_once(')')?.1;
        let mut fields = after.split_whitespace();
        let _state = fields.next()?;
        fields.next()?.parse().ok()
    }

    /// Every pid inside any root's subtree, tagged with that root's sid.
    pub(super) fn subtree_owners(
        roots: &[(String, u32)],
        table: &[(u32, u32)],
    ) -> Vec<(u32, String)> {
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (pid, ppid) in table {
            children.entry(*ppid).or_default().push(*pid);
        }
        let mut owners: Vec<(u32, String)> = Vec::new();
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        for (sid, root) in roots {
            let mut queue = vec![*root];
            while let Some(pid) = queue.pop() {
                if !seen.insert(pid) {
                    continue;
                }
                owners.push((pid, sid.clone()));
                if let Some(kids) = children.get(&pid) {
                    queue.extend(kids);
                }
            }
        }
        owners
    }

    /// Does `/proc/<pid>/fd` hold any of these socket inodes?
    fn pid_holds_socket(pid: u32, inodes: &[u64]) -> bool {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        for entry in entries.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let Some(target) = target.to_str() else {
                continue;
            };
            let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
            else {
                continue;
            };
            if inode
                .parse::<u64>()
                .ok()
                .is_some_and(|i| inodes.contains(&i))
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_replaces_by_sid_and_forget_drops() {
        let me = std::process::id();
        record("vp-t1", Some(me));
        record("vp-t1", Some(me)); // idempotent replace
        assert!(roots()
            .iter()
            .any(|(sid, pid)| sid == "vp-t1" && *pid == me));
        forget("vp-t1");
        assert!(!roots().iter().any(|(sid, _)| sid == "vp-t1"));
        record("vp-t1-none", None);
        assert!(!roots().iter().any(|(sid, _)| sid == "vp-t1-none"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dead_pids_are_pruned_on_read() {
        // A pid this box will not be running: the kernel's pid_max ceiling is
        // 4194304 and u32::MAX is far past it.
        record("vp-dead", Some(u32::MAX));
        assert!(!roots().iter().any(|(sid, _)| sid == "vp-dead"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_row_parses_only_established_loopback_on_the_asked_port() {
        use imp::parse_established_loopback_local;
        // 127.0.0.1:53724 -> 127.0.0.1:7788, ESTABLISHED, inode 1152048.
        let row = "   3: 0100007F:D1DC 0100007F:1E6C 01 00000000:00000000 00:00000000 00000000  1000        0 1152048 1 0000000000000000 20 4 30 10 -1";
        assert_eq!(parse_established_loopback_local(row, 0xD1DC), Some(1152048));
        assert_eq!(
            parse_established_loopback_local(row, 0xD1DD),
            None,
            "wrong port must not match"
        );
        let listening = row.replace(" 01 ", " 0A ");
        assert_eq!(
            parse_established_loopback_local(&listening, 0xD1DC),
            None,
            "only ESTABLISHED rows are a live client"
        );
        let external = row.replace("0100007F:D1DC", "0F02000A:D1DC");
        assert_eq!(
            parse_established_loopback_local(&external, 0xD1DC),
            None,
            "a non-loopback local address is not our peer"
        );
        // v6 ::1 with the same port, ESTABLISHED.
        let row6 = "   1: 00000000000000000000000001000000:D1DC 00000000000000000000000001000000:1E6C 01 00000000:00000000 00:00000000 00000000  1000        0 424242 1 0000000000000000 20 4 30 10 -1";
        assert_eq!(parse_established_loopback_local(row6, 0xD1DC), Some(424242));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_ppid_survives_a_hostile_comm() {
        use imp::parse_stat_ppid;
        assert_eq!(
            parse_stat_ppid("4242 (weird name) with) parens) S 137 4242 4242 0 -1"),
            Some(137)
        );
        assert_eq!(parse_stat_ppid("no parens at all"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn subtree_walk_tags_descendants_with_the_root_sid() {
        use imp::subtree_owners;
        // 100 -> 200 -> 300; 400 is unrelated.
        let table = vec![(200, 100), (300, 200), (400, 1)];
        let owners = subtree_owners(&[("s9".to_string(), 100)], &table);
        let pids: Vec<u32> = owners.iter().map(|(pid, _)| *pid).collect();
        assert!(pids.contains(&100) && pids.contains(&200) && pids.contains(&300));
        assert!(!pids.contains(&400));
        assert!(owners.iter().all(|(_, sid)| sid == "s9"));
    }

    /// End to end against the real `/proc`: this test process plays the vendor
    /// — it is registered as a root and then CONNECTS to a local listener, so
    /// resolving the client port must find it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_loopback_client_resolves_to_the_registered_root_that_owns_it() {
        use std::net::{TcpListener, TcpStream};
        let me = std::process::id();
        record("vp-e2e", Some(me));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let stream = TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
        let client_port = stream.local_addr().unwrap().port();
        let resolved = owner_of_local_peer(client_port).expect("our own connection must resolve");
        // Parallel tests may register this same pid under another sid; any sid
        // that maps to OUR pid is a correct answer.
        let owned_by_me: Vec<String> = roots()
            .into_iter()
            .filter(|(_, pid)| *pid == me)
            .map(|(sid, _)| sid)
            .collect();
        assert!(
            owned_by_me.contains(&resolved),
            "resolved {resolved} not among this process's sids {owned_by_me:?}"
        );
        forget("vp-e2e");
    }
}
