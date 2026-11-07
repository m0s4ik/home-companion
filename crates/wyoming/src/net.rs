// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A tiny address-or-network type for the allow-list (`"192.0.2.10"`, `"192.0.2.0/24"`,
//! `"2001:db8::/32"`).

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// An IP address (prefix = full length) or a CIDR network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    /// Parses `"a.b.c.d"`, `"a.b.c.d/len"`, an IPv6 address or `"v6/len"`. Host bits set in a CIDR
    /// are accepted and ignored (`192.0.2.7/24` = `192.0.2.0/24`).
    pub fn parse(s: &str) -> Result<Net, String> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a
            .parse()
            .map_err(|_| format!("{s:?} is not an IP address or network"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => {
                if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(format!("{s:?}: bad prefix length"));
                }
                let n: u8 = p.parse().map_err(|_| format!("{s:?}: bad prefix length"))?;
                if n > max {
                    return Err(format!("{s:?}: prefix length over {max}"));
                }
                n
            }
        };
        Ok(Net { addr, prefix })
    }

    /// The network address as written.
    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// True when `ip` is in the network. IPv4-mapped IPv6 addresses (`::ffff:192.0.2.10`, what a
    /// dual-stack `[::]` listener reports for IPv4 peers) are compared as IPv4.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        // An IPv4-mapped network (`::ffff:a.b.c.d/len`, len >= 96) is the IPv4 network len - 96.
        let (net, prefix) = match self.addr {
            IpAddr::V6(v6) if self.prefix >= 96 => match v6.to_ipv4_mapped() {
                Some(v4) => (IpAddr::V4(v4), self.prefix - 96),
                None => (self.addr, self.prefix),
            },
            a => (a, self.prefix),
        };
        match (net, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let p = u32::from(prefix);
                let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let p = u32::from(prefix);
                let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for Net {
    type Err = String;
    fn from_str(s: &str) -> Result<Net, String> {
        Net::parse(s)
    }
}

impl From<IpAddr> for Net {
    fn from(addr: IpAddr) -> Net {
        Net {
            addr,
            prefix: if addr.is_ipv4() { 32 } else { 128 },
        }
    }
}

impl fmt::Display for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let max = if self.addr.is_ipv4() { 32 } else { 128 };
        if self.prefix == max {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix)
        }
    }
}

/// True when `ip` is in any of `nets`.
pub fn allowed(nets: &[Net], ip: &IpAddr) -> bool {
    nets.iter().any(|n| n.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn v4() {
        let n = Net::parse("192.0.2.10").unwrap();
        assert!(n.contains(&ip("192.0.2.10")));
        assert!(!n.contains(&ip("192.0.2.11")));
        let n = Net::parse("192.0.2.0/24").unwrap();
        assert!(n.contains(&ip("192.0.2.0")) && n.contains(&ip("192.0.2.255")));
        assert!(!n.contains(&ip("192.0.3.1")));
        assert!(n.contains(&ip("::ffff:192.0.2.9")), "IPv4-mapped");
        assert!(!n.contains(&ip("2001:db8::1")));
        let all = Net::parse("0.0.0.0/0").unwrap();
        assert!(all.contains(&ip("203.0.113.5")));
        assert!(!all.contains(&ip("::1")));
        let host_bits = Net::parse("192.0.2.77/30").unwrap();
        assert!(host_bits.contains(&ip("192.0.2.76")) && !host_bits.contains(&ip("192.0.2.80")));
        assert!(
            Net::parse("198.51.100.1/32")
                .unwrap()
                .contains(&ip("198.51.100.1"))
        );
        assert!(
            Net::parse("198.51.100.1/31")
                .unwrap()
                .contains(&ip("198.51.100.0"))
        );
    }

    #[test]
    fn v6() {
        let n = Net::parse("2001:db8::/32").unwrap();
        assert!(n.contains(&ip("2001:db8:ffff::1")));
        assert!(!n.contains(&ip("2001:db9::1")));
        assert!(!n.contains(&ip("192.0.2.1")));
        assert!(Net::parse("::1").unwrap().contains(&ip("::1")));
        assert!(Net::parse("::/0").unwrap().contains(&ip("fe80::1")));
        assert!(
            Net::parse("2001:db8::1/128")
                .unwrap()
                .contains(&ip("2001:db8::1"))
        );
        // a mapped network matches plain IPv4 peers
        let mapped = Net::parse("::ffff:192.0.2.0/120").unwrap();
        assert!(mapped.contains(&ip("192.0.2.200")) && !mapped.contains(&ip("192.0.3.1")));
    }

    #[test]
    fn garbage() {
        for s in [
            "",
            "/",
            "192.0.2.0/",
            "192.0.2.0/33",
            "192.0.2.0/-1",
            "192.0.2.0/+8",
            "192.0.2.0/a",
            "host.example.home",
            "192.0.2",
            "256.0.0.1",
            "::1/129",
            "1.2.3.4/24/1",
            "192.0.2.1 /24",
        ] {
            assert!(Net::parse(s).is_err(), "{s:?} accepted");
        }
        assert_eq!(Net::parse(" 192.0.2.1 ").unwrap().to_string(), "192.0.2.1");
        assert_eq!(
            Net::parse("192.0.2.0/24").unwrap().to_string(),
            "192.0.2.0/24"
        );
    }
}
