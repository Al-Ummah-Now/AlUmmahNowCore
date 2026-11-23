//! App versions (wiki AUN-APP-UPDATES §2.1): 3 or 4 numeric parts (a missing 4th part is 0), an
//! optional semver pre-release (`-preview.3`) and optional build metadata (`+abc`, ignored).

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    nums: [u64; 4],
    pre: Option<Vec<String>>,
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn parse(v: &str) -> Option<Parsed> {
    let (core_pre, _build) = match v.split_once('+') {
        Some((a, b))
            if !b.is_empty()
                && b.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') =>
        {
            (a, Some(b))
        }
        Some(_) => return None,
        None => (v, None),
    };
    let (core, pre) = match core_pre.split_once('-') {
        Some((a, b)) => {
            let ids: Vec<String> = b.split('.').map(str::to_owned).collect();
            if !ids.iter().all(|i| ident_ok(i)) {
                return None;
            }
            (a, Some(ids))
        }
        None => (core_pre, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let mut nums = [0u64; 4];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 6 || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    Some(Parsed { nums, pre })
}

/// Whether `v` is a version the API accepts.
pub fn is_valid(v: &str) -> bool {
    parse(v).is_some()
}

fn cmp_pre(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            for i in 0..x.len().max(y.len()) {
                let (Some(p), Some(q)) = (x.get(i), y.get(i)) else {
                    return x.len().cmp(&y.len());
                };
                let np = p.chars().all(|c| c.is_ascii_digit());
                let nq = q.chars().all(|c| c.is_ascii_digit());
                let o = match (np, nq) {
                    (true, true) => p.parse::<u64>().unwrap_or(0).cmp(&q.parse::<u64>().unwrap_or(0)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => p.cmp(q),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        }
    }
}

/// Semver precedence over 4 numeric parts; `None` when either version is malformed.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (pa, pb) = (parse(a)?, parse(b)?);
    Some(pa.nums.cmp(&pb.nums).then_with(|| cmp_pre(&pa.pre, &pb.pre)))
}

/// The numeric parts only (`1.4.0-beta.1` counts as `1.4.0`): feature-flag rules use this.
pub fn compare_cores(a: &str, b: &str) -> Option<Ordering> {
    Some(parse(a)?.nums.cmp(&parse(b)?.nums))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        for ok in [
            "1.2.3",
            "2026.11.22.2",
            "1.2.3-preview.1",
            "1.2.3+abc",
            "1.2.3-rc.1+abc",
        ] {
            assert!(is_valid(ok), "{ok}");
        }
        for bad in [
            "1.2",
            "1.2.3.4.5",
            "1.2.3-",
            "1.2.3-a..b",
            "v1.2.3",
            "1.2.3 ",
            "1234567.0.0",
        ] {
            assert!(!is_valid(bad), "{bad}");
        }
    }
}
