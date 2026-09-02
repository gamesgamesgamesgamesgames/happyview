//! Host shape: cores, memory, container limits, disk, network.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::maintenance::disk;

/// Parse `anon` and `file` out of a cgroup v2 `memory.stat`.
pub fn parse_memory_stat(contents: &str) -> Option<(u64, u64)> {
    let mut anon = None;
    let mut file = None;

    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("anon"), Some(v)) => anon = v.parse::<u64>().ok(),
            (Some("file"), Some(v)) => file = v.parse::<u64>().ok(),
            _ => {}
        }
    }

    Some((anon?, file?))
}

fn read_memory() -> Option<(u64, u64)> {
    let contents = std::fs::read_to_string("/sys/fs/cgroup/memory.stat").ok()?;
    parse_memory_stat(&contents)
}

fn read_memory_limit() -> Option<u64> {
    let raw = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    raw.trim().parse::<u64>().ok()
}

/// Parse total receive/transmit bytes out of `/proc/net/dev`, summed across
/// every interface except `lo`.
pub fn parse_net_dev(contents: &str) -> Option<(u64, u64)> {
    let mut rx_total: u64 = 0;
    let mut tx_total: u64 = 0;
    let mut found_any = false;

    for line in contents.lines() {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let iface = iface.trim();
        if iface.is_empty() {
            continue;
        }

        let fields: Vec<&str> = rest.split_whitespace().collect();
        let (Some(rx_field), Some(tx_field)) = (fields.first(), fields.get(8)) else {
            continue;
        };
        let (Ok(rx), Ok(tx)) = (rx_field.parse::<u64>(), tx_field.parse::<u64>()) else {
            continue;
        };

        found_any = true;
        if iface != "lo" {
            rx_total += rx;
            tx_total += tx;
        }
    }

    if found_any {
        Some((rx_total, tx_total))
    } else {
        None
    }
}

fn read_net_bytes() -> Option<(u64, u64)> {
    let contents = std::fs::read_to_string("/proc/net/dev").ok()?;
    parse_net_dev(&contents)
}

pub fn report(db_url: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();

    if let Ok(cores) = std::thread::available_parallelism() {
        out.insert("cpu_cores".to_string(), json!(cores.get()));
    }

    if let Some((anon, file)) = read_memory() {
        out.insert("memory_anon_bytes".to_string(), json!(anon));
        out.insert("memory_file_bytes".to_string(), json!(file));
    }

    if let Some(limit) = read_memory_limit() {
        out.insert("memory_limit_bytes".to_string(), json!(limit));
    }
    out.insert(
        "containerised".to_string(),
        json!(std::path::Path::new("/sys/fs/cgroup/memory.max").exists()),
    );

    if let Some((rx, tx)) = read_net_bytes() {
        out.insert("net_rx_bytes".to_string(), json!(rx));
        out.insert("net_tx_bytes".to_string(), json!(tx));
    }

    if let Some(r) = disk::report(db_url) {
        if std::path::Path::new(&r.db_path).exists() {
            out.insert("db_bytes".to_string(), json!(r.db_bytes));
            out.insert("wal_bytes".to_string(), json!(r.wal_bytes));
        }
        if let Some(free) = r.db_fs_free {
            out.insert("db_fs_free_bytes".to_string(), json!(free));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anon_and_file_from_cgroup_memory_stat() {
        let stat = "anon 524288000\nfile 1073741824\nkernel_stack 65536\nslab 12345\n";
        assert_eq!(parse_memory_stat(stat), Some((524_288_000, 1_073_741_824)));
    }

    #[test]
    fn returns_none_when_anon_is_missing() {
        assert_eq!(parse_memory_stat("file 1024\nslab 1\n"), None);
    }

    #[test]
    fn ignores_unparseable_values_rather_than_panicking() {
        assert_eq!(parse_memory_stat("anon not-a-number\nfile 1024\n"), None);
    }

    #[test]
    fn tolerates_blank_lines_and_trailing_whitespace() {
        let stat = "\nanon 100  \n\nfile 200\n";
        assert_eq!(parse_memory_stat(stat), Some((100, 200)));
    }

    #[test]
    fn sums_receive_and_transmit_bytes_excluding_loopback() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                    face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n \
                      lo:  123456     789    0    0    0     0          0         0   123456     789    0    0    0     0       0          0\n \
                    eth0: 9876543   54321    0    0    0     0          0         0  1234567   65432    0    0    0     0       0          0\n";
        assert_eq!(parse_net_dev(dev), Some((9_876_543, 1_234_567)));
    }

    #[test]
    fn returns_zero_sum_rather_than_none_when_only_loopback_is_present() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                    face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n \
                      lo:  123456     789    0    0    0     0          0         0   123456     789    0    0    0     0       0          0\n";
        assert_eq!(parse_net_dev(dev), Some((0, 0)));
    }

    #[test]
    fn skips_a_line_with_too_few_fields_rather_than_erroring() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                    face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n \
                    eth0: 1000 2000\n \
                    eth1:  500     10    0    0    0     0          0         0      700     10    0    0    0     0       0          0\n";
        assert_eq!(parse_net_dev(dev), Some((500, 700)));
    }

    #[test]
    fn returns_none_when_no_interface_line_parses_at_all() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                    face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n \
                    eth0: not-a-number 2000    0    0    0     0          0         0 also-not-a-number 10    0    0    0     0       0          0\n";
        assert_eq!(parse_net_dev(dev), None);
    }

    #[test]
    fn report_always_names_the_core_count() {
        let host = report("sqlite:/tmp/does-not-exist.db");
        assert!(host.contains_key("cpu_cores"));
    }

    #[test]
    fn report_omits_metrics_it_cannot_measure_rather_than_reporting_zero() {
        let host = report("sqlite:/tmp/does-not-exist.db");
        for key in ["db_bytes", "wal_bytes"] {
            assert!(!host.contains_key(key), "{key} must be absent, not zeroed");
        }
    }
}
