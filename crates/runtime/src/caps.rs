use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use camino::Utf8PathBuf;
use toml::Value;

use crate::error::Error;

/// Filesystem capability entry describing a preopened root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FsCapability {
    pub root: Utf8PathBuf,
    pub write: bool,
}

impl FsCapability {
    pub fn new(root: Utf8PathBuf, write: bool) -> Self {
        Self { root, write }
    }
}

/// KB/model capability sets – all, none, or limited domain lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CapabilitySet {
    #[default]
    None,
    All,
    Domains(BTreeSet<String>),
}

impl CapabilitySet {
    pub fn intersects(&self, other: &Self) -> Self {
        match (self, other) {
            (CapabilitySet::None, _) | (_, CapabilitySet::None) => CapabilitySet::None,
            (CapabilitySet::All, rhs) => rhs.clone(),
            (lhs, CapabilitySet::All) => lhs.clone(),
            (CapabilitySet::Domains(lhs), CapabilitySet::Domains(rhs)) => {
                let domains = lhs.intersection(rhs).cloned().collect::<BTreeSet<_>>();
                if domains.is_empty() {
                    CapabilitySet::None
                } else {
                    CapabilitySet::Domains(domains)
                }
            }
        }
    }

    pub fn permits(&self, domain: &str) -> bool {
        match self {
            CapabilitySet::None => false,
            CapabilitySet::All => true,
            CapabilitySet::Domains(set) => set.contains(domain),
        }
    }
}

/// Network destination (host + optional port).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetLocation {
    pub host: String,
    pub port: Option<u16>,
}

impl NetLocation {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        if raw.trim().is_empty() {
            return Err(Error::InvalidNetworkHost(raw.to_string()));
        }
        if let Some((host, port_raw)) = raw.split_once(':') {
            let port = port_raw
                .parse::<u16>()
                .map_err(|_| Error::InvalidNetworkHost(raw.to_string()))?;
            Ok(Self {
                host: host.trim().to_string(),
                port: Some(port),
            })
        } else {
            Ok(Self {
                host: raw.trim().to_string(),
                port: None,
            })
        }
    }

    pub fn matches(&self, addr: &SocketAddr) -> bool {
        if let Some(port) = self.port
            && port != addr.port()
        {
            return false;
        }
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            return ip == addr.ip();
        }
        let port = self.port.unwrap_or(addr.port());
        if let Ok(iter) = (self.host.as_str(), port).to_socket_addrs() {
            for candidate in iter {
                if candidate.ip() == addr.ip() {
                    return true;
                }
            }
        }
        false
    }
}

/// Effective capability set for an agent at runtime.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub fs: Vec<FsCapability>,
    pub net_hosts: Vec<NetLocation>,
    pub net_allow_http: bool,
    pub time: bool,
    pub kb_read: CapabilitySet,
    pub kb_write: CapabilitySet,
    pub model: bool,
    pub secrets: BTreeSet<String>,
    pub exec: bool,
}

impl Caps {
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn permits_secret(&self, secret_id: &str) -> bool {
        self.secrets.contains(secret_id)
    }

    pub fn permits_host(&self, addr: &SocketAddr) -> bool {
        if self.net_hosts.is_empty() {
            return false;
        }
        self.net_hosts.iter().any(|host| host.matches(addr))
    }

    pub fn permits_port(&self, port: u16) -> bool {
        if port == 80 && !self.net_allow_http {
            return false;
        }
        true
    }

    pub fn from_policy(value: &Value) -> Result<CapsParse, Error> {
        let table = value
            .get("capabilities")
            .and_then(Value::as_table)
            .ok_or(Error::InvalidPolicyFormat)?;

        let fs_entries = parse_fs(table)?;
        let (net_hosts, net_allow_http, net_present, net_http_present) = parse_net(table)?;

        let (time, time_present) = parse_bool(table.get("time"));
        let (model, model_present) = parse_bool(table.get("model"));
        let (exec, exec_present) = parse_bool(table.get("exec"));

        let (kb_read, kb_read_present) = parse_capability_set(table.get("kb_read"))?;
        let (kb_write, kb_write_present) = parse_capability_set(table.get("kb_write"))?;

        let (secrets, secrets_present) = parse_secrets(table.get("secrets"));

        Ok(CapsParse {
            caps: Caps {
                fs: fs_entries.items,
                net_hosts,
                net_allow_http,
                time,
                kb_read,
                kb_write,
                model,
                secrets,
                exec,
            },
            presence: CapsPresence {
                fs: fs_entries.present,
                net: net_present,
                net_allow_http: net_http_present,
                time: time_present,
                kb_read: kb_read_present,
                kb_write: kb_write_present,
                model: model_present,
                secrets: secrets_present,
                exec: exec_present,
            },
        })
    }

    pub fn intersect_with(&self, overrides: &Caps, presence: &CapsPresence) -> Caps {
        Caps {
            fs: merge_fs(&self.fs, &overrides.fs, presence.fs),
            net_hosts: merge_hosts(&self.net_hosts, &overrides.net_hosts, presence.net),
            net_allow_http: if presence.net_allow_http {
                self.net_allow_http && overrides.net_allow_http
            } else {
                self.net_allow_http
            },
            time: if presence.time {
                self.time && overrides.time
            } else {
                self.time
            },
            kb_read: if presence.kb_read {
                self.kb_read.intersects(&overrides.kb_read)
            } else {
                self.kb_read.clone()
            },
            kb_write: if presence.kb_write {
                self.kb_write.intersects(&overrides.kb_write)
            } else {
                self.kb_write.clone()
            },
            model: if presence.model {
                self.model && overrides.model
            } else {
                self.model
            },
            secrets: intersect_secrets(&self.secrets, &overrides.secrets, presence.secrets),
            exec: if presence.exec {
                self.exec && overrides.exec
            } else {
                self.exec
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapsPresence {
    pub fs: bool,
    pub net: bool,
    pub net_allow_http: bool,
    pub time: bool,
    pub kb_read: bool,
    pub kb_write: bool,
    pub model: bool,
    pub secrets: bool,
    pub exec: bool,
}

#[derive(Debug, Clone)]
pub struct CapsParse {
    pub caps: Caps,
    pub presence: CapsPresence,
}

fn parse_fs(table: &toml::map::Map<String, Value>) -> Result<FsParseResult, Error> {
    let mut fs_entries = Vec::new();
    let mut present = false;
    if let Some(Value::Array(entries)) = table.get("fs") {
        present = true;
        for value in entries {
            fs_entries.push(parse_fs_entry(value, false)?);
        }
    }
    if let Some(Value::Array(entries)) = table.get("fs_ro") {
        present = true;
        for value in entries {
            fs_entries.push(parse_fs_entry(value, false)?);
        }
    }
    if let Some(Value::Array(entries)) = table.get("fs_rw") {
        present = true;
        for value in entries {
            fs_entries.push(parse_fs_entry(value, true)?);
        }
    }
    dedupe_fs(&mut fs_entries);
    Ok(FsParseResult {
        items: fs_entries,
        present,
    })
}

struct FsParseResult {
    items: Vec<FsCapability>,
    present: bool,
}

fn parse_bool(value: Option<&Value>) -> (bool, bool) {
    match value {
        Some(Value::Boolean(b)) => (*b, true),
        Some(_) => (false, true),
        None => (false, false),
    }
}

fn parse_capability_set(value: Option<&Value>) -> Result<(CapabilitySet, bool), Error> {
    match value {
        None => Ok((CapabilitySet::None, false)),
        Some(Value::Boolean(flag)) => {
            if *flag {
                Ok((CapabilitySet::All, true))
            } else {
                Ok((CapabilitySet::None, true))
            }
        }
        Some(Value::Array(entries)) => {
            let mut items = BTreeSet::new();
            for entry in entries {
                let Some(domain) = entry.as_str() else {
                    return Err(Error::InvalidCapabilityEntry(format!("{entry:?}")));
                };
                if !domain.trim().is_empty() {
                    items.insert(domain.trim().to_string());
                }
            }
            if items.is_empty() {
                Ok((CapabilitySet::None, true))
            } else {
                Ok((CapabilitySet::Domains(items), true))
            }
        }
        Some(other) => Err(Error::InvalidCapabilityEntry(format!("{other:?}"))),
    }
}

fn parse_secrets(value: Option<&Value>) -> (BTreeSet<String>, bool) {
    match value {
        Some(Value::Array(arr)) => (
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect(),
            true,
        ),
        Some(_) => (BTreeSet::new(), true),
        None => (BTreeSet::new(), false),
    }
}

fn parse_net(
    table: &toml::map::Map<String, Value>,
) -> Result<(Vec<NetLocation>, bool, bool, bool), Error> {
    let mut present = false;
    let mut allow_http_present = false;
    match table.get("net") {
        None => Ok((Vec::new(), false, present, allow_http_present)),
        Some(Value::Array(entries)) => {
            present = true;
            let mut hosts = Vec::new();
            for entry in entries {
                let Some(raw) = entry.as_str() else {
                    return Err(Error::InvalidNetworkHost(format!("{entry:?}")));
                };
                hosts.push(NetLocation::parse(raw)?);
            }
            dedupe_hosts(&mut hosts);
            Ok((hosts, false, present, allow_http_present))
        }
        Some(Value::Table(tbl)) => {
            let hosts_value = tbl
                .get("hosts")
                .ok_or_else(|| Error::InvalidNetworkHost("missing net.hosts".into()))?;
            let host_list = hosts_value
                .as_array()
                .ok_or_else(|| Error::InvalidNetworkHost("net.hosts must be array".into()))?;
            let mut hosts = Vec::new();
            for entry in host_list {
                let Some(raw) = entry.as_str() else {
                    return Err(Error::InvalidNetworkHost(format!("{entry:?}")));
                };
                hosts.push(NetLocation::parse(raw)?);
            }
            dedupe_hosts(&mut hosts);
            present = true;
            if tbl.contains_key("allow_http") {
                allow_http_present = true;
            }
            let allow_http = tbl
                .get("allow_http")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok((hosts, allow_http, present, allow_http_present))
        }
        Some(other) => Err(Error::InvalidNetworkHost(format!("{other:?}"))),
    }
}

fn parse_fs_entry(value: &Value, write: bool) -> Result<FsCapability, Error> {
    let raw = value
        .as_str()
        .ok_or_else(|| Error::InvalidFsEntry(format!("{value:?}")))?;
    if raw.trim().is_empty() {
        return Err(Error::InvalidFsEntry(raw.to_string()));
    }
    let path = Utf8PathBuf::from(raw);
    Ok(FsCapability::new(path, write))
}

fn dedupe_fs(entries: &mut Vec<FsCapability>) {
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.clone()));
}

fn dedupe_hosts(entries: &mut Vec<NetLocation>) {
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.clone()));
}

fn merge_fs(base: &[FsCapability], overrides: &[FsCapability], present: bool) -> Vec<FsCapability> {
    if !present {
        return base.to_vec();
    }
    let mut map = HashMap::new();
    for entry in overrides {
        map.insert(entry.root.clone(), entry.write);
    }
    let mut result = Vec::new();
    for entry in base {
        if let Some(write) = map.get(&entry.root) {
            result.push(FsCapability {
                root: entry.root.clone(),
                write: entry.write && *write,
            });
        }
    }
    result
}

fn merge_hosts(base: &[NetLocation], overrides: &[NetLocation], present: bool) -> Vec<NetLocation> {
    if !present {
        return base.to_vec();
    }
    let mut by_host: BTreeMap<String, Vec<Option<u16>>> = BTreeMap::new();
    for entry in overrides {
        by_host
            .entry(entry.host.clone())
            .or_default()
            .push(entry.port);
    }
    let mut result = Vec::new();
    for entry in base {
        if let Some(ports) = by_host.get(&entry.host) {
            if ports.iter().any(|p| p.is_none()) {
                result.push(entry.clone());
                continue;
            }
            match entry.port {
                Some(port) => {
                    if ports.contains(&Some(port)) {
                        result.push(entry.clone());
                    }
                }
                None => {
                    for port in ports.iter().flatten() {
                        result.push(NetLocation {
                            host: entry.host.clone(),
                            port: Some(*port),
                        });
                    }
                }
            }
        }
    }
    dedupe_hosts(&mut result);
    result
}

fn intersect_secrets(
    base: &BTreeSet<String>,
    overrides: &BTreeSet<String>,
    present: bool,
) -> BTreeSet<String> {
    if !present {
        return base.clone();
    }
    base.intersection(overrides).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_caps(toml_src: &str) -> CapsParse {
        let value: Value = toml::from_str(toml_src).unwrap();
        Caps::from_policy(&value).unwrap()
    }

    #[test]
    fn fs_override_downgrades_write() {
        let base = parse_caps(
            r#"[capabilities]
fs_rw = ["/data"]
"#,
        );
        let overrides = parse_caps(
            r#"[capabilities]
fs_ro = ["/data"]
"#,
        );
        let merged = base
            .caps
            .intersect_with(&overrides.caps, &overrides.presence);
        assert_eq!(merged.fs.len(), 1);
        assert!(!merged.fs[0].write);
    }

    #[test]
    fn fs_override_drops_unlisted_paths() {
        let base = parse_caps(
            r#"[capabilities]
fs_rw = ["/data", "/logs"]
"#,
        );
        let overrides = parse_caps(
            r#"[capabilities]
fs_rw = ["/logs"]
"#,
        );
        let merged = base
            .caps
            .intersect_with(&overrides.caps, &overrides.presence);
        assert_eq!(merged.fs.len(), 1);
        assert_eq!(merged.fs[0].root.as_str(), "/logs");
    }

    #[test]
    fn net_override_tightens_port() {
        let base = parse_caps(
            r#"[capabilities]
net = ["example.com"]
"#,
        );
        let overrides = parse_caps(
            r#"[capabilities]
net = ["example.com:443"]
"#,
        );
        let merged = base
            .caps
            .intersect_with(&overrides.caps, &overrides.presence);
        assert_eq!(merged.net_hosts.len(), 1);
        assert_eq!(merged.net_hosts[0].port, Some(443));
    }

    #[test]
    fn override_leaves_missing_booleans_untouched() {
        let base = parse_caps(
            r#"[capabilities]
time = true
model = true
exec = true
"#,
        );
        let overrides = parse_caps(
            r#"[capabilities]
fs = []
"#,
        );
        let merged = base
            .caps
            .intersect_with(&overrides.caps, &overrides.presence);
        assert!(merged.time);
        assert!(merged.model);
        assert!(merged.exec);
    }
}
