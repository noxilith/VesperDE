//! Removable drives (USB sticks, SD cards): found with `lsblk`, mounted, unmounted and ejected with `udisksctl`
//! (the udisks2 service, which lets a normal user do this).

use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    /// The device node, e.g. `/dev/sdb1`.
    pub path: String,
    /// The label, or a generic name when it has none.
    pub label: String,
    pub size: String,
    pub mountpoint: Option<String>,
    /// The whole disk it belongs to (what gets powered off on eject).
    pub disk: String,
}

/// The removable drives that hold a file system, from `lsblk -J` output.
pub fn parse(json: &str) -> Vec<Drive> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let mut out = Vec::new();
    for disk in v["blockdevices"].as_array().into_iter().flatten() {
        let disk_path = disk["path"].as_str().unwrap_or_default().to_string();
        let removable = |n: &serde_json::Value| n["rm"].as_bool().unwrap_or(false) || n["hotplug"].as_bool().unwrap_or(false);
        let mut nodes: Vec<&serde_json::Value> = disk["children"].as_array().into_iter().flatten().collect();
        if nodes.is_empty() {
            nodes.push(disk); // a stick formatted without a partition table
        }
        for n in nodes {
            let fstype = n["fstype"].as_str().unwrap_or_default();
            if !(removable(n) || removable(disk)) || fstype.is_empty() || matches!(fstype, "swap" | "crypto_LUKS" | "LVM2_member" | "linux_raid_member") {
                continue;
            }
            let path = n["path"].as_str().unwrap_or_default().to_string();
            let size = n["size"].as_str().unwrap_or_default().to_string();
            let label = n["label"].as_str().filter(|l| !l.is_empty()).map_or_else(|| format!("Drive {size}"), str::to_string);
            out.push(Drive { path, label, size, mountpoint: n["mountpoint"].as_str().map(str::to_string), disk: disk_path.clone() });
        }
    }
    out
}

pub fn list() -> Vec<Drive> {
    match crate::cmd::output(Command::new("lsblk").args(["-J", "-o", "NAME,PATH,LABEL,SIZE,FSTYPE,MOUNTPOINT,RM,HOTPLUG,TYPE"])) {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Mount (when needed) and open the drive in the file manager, off the UI thread.
pub fn open(d: Drive) {
    std::thread::spawn(move || {
        let mount = match d.mountpoint {
            Some(m) => Some(m),
            None => udisks(&["mount", "-b", &d.path]).and_then(|out| {
                // "Mounted /dev/sdb1 at /run/media/user/LABEL."
                out.split(" at ").nth(1).map(|s| s.trim().trim_end_matches('.').to_string())
            }),
        };
        if let Some(m) = mount {
            let _ = Command::new("xdg-open").arg(m).spawn().map(|mut c| c.wait());
        }
    });
}

/// Unmount and power the drive off so it is safe to pull out.
pub fn eject(d: Drive) {
    std::thread::spawn(move || {
        if d.mountpoint.is_some() {
            udisks(&["unmount", "-b", &d.path]);
        }
        udisks(&["power-off", "-b", &d.disk]);
    });
}

fn udisks(args: &[&str]) -> Option<String> {
    match Command::new("udisksctl").args(args).output() {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            tracing::warn!("udisksctl {args:?}: {}", String::from_utf8_lossy(&o.stderr).trim());
            None
        }
        Err(_) => {
            tracing::warn!("udisksctl (udisks2) is not installed: removable drives cannot be mounted");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_usb_partitions_only() {
        let json = r#"{"blockdevices":[
          {"name":"nvme0n1","path":"/dev/nvme0n1","label":null,"size":"953G","fstype":null,"mountpoint":null,"rm":false,"hotplug":false,"type":"disk","children":[
            {"name":"nvme0n1p1","path":"/dev/nvme0n1p1","label":null,"size":"2G","fstype":"vfat","mountpoint":"/boot","rm":false,"hotplug":false,"type":"part"}]},
          {"name":"sdb","path":"/dev/sdb","label":null,"size":"29G","fstype":null,"mountpoint":null,"rm":true,"hotplug":true,"type":"disk","children":[
            {"name":"sdb1","path":"/dev/sdb1","label":"STICK","size":"29G","fstype":"exfat","mountpoint":"/run/media/u/STICK","rm":true,"hotplug":true,"type":"part"},
            {"name":"sdb2","path":"/dev/sdb2","label":null,"size":"1G","fstype":"swap","mountpoint":null,"rm":true,"hotplug":true,"type":"part"}]}]}"#;
        let d = parse(json);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].path.as_str(), d[0].label.as_str(), d[0].disk.as_str()), ("/dev/sdb1", "STICK", "/dev/sdb"));
        assert_eq!(d[0].mountpoint.as_deref(), Some("/run/media/u/STICK"));
    }
}
