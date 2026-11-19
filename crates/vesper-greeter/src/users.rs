//! The accounts offered on the login screen: real people, not system users like `greeter`.

use std::fs;

/// Lowest and highest user id that counts as a person (`UID_MIN` from `/etc/login.defs`, default 1000).
fn uid_range(login_defs: &str) -> (u32, u32) {
    let get = |key: &str, default: u32| {
        login_defs
            .lines()
            .filter_map(|l| {
                let mut w = l.split_whitespace();
                (w.next() == Some(key)).then(|| w.next()?.parse().ok()).flatten()
            })
            .next()
            .unwrap_or(default)
    };
    (get("UID_MIN", 1000), get("UID_MAX", 60000))
}

/// Login names from `passwd` text: uid in range, a shell you can log in with, and not `greeter` or `nobody`.
pub fn parse(passwd: &str, login_defs: &str) -> Vec<String> {
    let (min, max) = uid_range(login_defs);
    let mut out: Vec<(u32, String)> = Vec::new();
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        let Ok(uid) = f[2].parse::<u32>() else { continue };
        let shell = f[6].trim();
        let no_login = shell.is_empty() || shell.ends_with("/nologin") || shell.ends_with("/false");
        if uid < min || uid > max || no_login || matches!(f[0], "greeter" | "nobody" | "greetd") || f[0].starts_with("_") {
            continue;
        }
        out.push((uid, f[0].to_string()));
    }
    out.sort();
    out.into_iter().map(|(_, n)| n).collect()
}

pub fn list() -> Vec<String> {
    let passwd = fs::read_to_string(std::env::var("VESPER_PASSWD").unwrap_or_else(|_| "/etc/passwd".into())).unwrap_or_default();
    parse(&passwd, &fs::read_to_string("/etc/login.defs").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_people_and_drops_system_accounts() {
        let passwd = "root:x:0:0::/root:/bin/bash\n\
            greeter:x:1002:1002::/var/lib/greeter:/bin/bash\n\
            bob:x:1001:1001::/home/bob:/usr/bin/fish\n\
            alice:x:1000:1000::/home/alice:/bin/bash\n\
            svc:x:1500:1500::/srv:/usr/bin/nologin\n\
            nobody:x:65534:65534::/:/usr/bin/nologin\n";
        assert_eq!(parse(passwd, ""), vec!["alice", "bob"]);
        assert_eq!(parse(passwd, "UID_MIN 1001\n"), vec!["bob"]);
    }
}
