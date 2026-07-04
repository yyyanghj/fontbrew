use crate::PackageVersion;
use semver::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionComparison {
    Equal,
    CandidateIsNewer,
    CurrentIsNewer,
    Unknown,
}

pub fn compare_versions(current: &PackageVersion, candidate: &PackageVersion) -> VersionComparison {
    let current = normalize_version(current.as_str());
    let candidate = normalize_version(candidate.as_str());

    if current == candidate {
        return VersionComparison::Equal;
    }

    if let (Some(current_date), Some(candidate_date)) =
        (parse_date_like(current), parse_date_like(candidate))
    {
        return compare_ordering(current_date.cmp(&candidate_date));
    }

    if looks_date_like(current) || looks_date_like(candidate) {
        return VersionComparison::Unknown;
    }

    if let (Some(current_semver), Some(candidate_semver)) =
        (parse_semver(current), parse_semver(candidate))
    {
        return compare_ordering(current_semver.cmp_precedence(&candidate_semver));
    }

    if let (Some(current_numbers), Some(candidate_numbers)) = (
        parse_numeric_sequence(current),
        parse_numeric_sequence(candidate),
    ) {
        return compare_numeric_sequences(&current_numbers, &candidate_numbers);
    }

    VersionComparison::Unknown
}

fn normalize_version(version: &str) -> &str {
    let trimmed = version.trim();

    if let Some(stripped) = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
    {
        if stripped.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return stripped;
        }
    }

    trimmed
}

fn compare_ordering(ordering: std::cmp::Ordering) -> VersionComparison {
    match ordering {
        std::cmp::Ordering::Less => VersionComparison::CandidateIsNewer,
        std::cmp::Ordering::Equal => VersionComparison::Equal,
        std::cmp::Ordering::Greater => VersionComparison::CurrentIsNewer,
    }
}

fn compare_numeric_sequences(current: &[u64], candidate: &[u64]) -> VersionComparison {
    let length = current.len().max(candidate.len());

    for index in 0..length {
        let current_part = current.get(index).copied().unwrap_or(0);
        let candidate_part = candidate.get(index).copied().unwrap_or(0);

        match current_part.cmp(&candidate_part) {
            std::cmp::Ordering::Less => return VersionComparison::CandidateIsNewer,
            std::cmp::Ordering::Greater => return VersionComparison::CurrentIsNewer,
            std::cmp::Ordering::Equal => {}
        }
    }

    VersionComparison::Equal
}

fn parse_semver(version: &str) -> Option<Version> {
    Version::parse(version).ok()
}

fn parse_numeric_sequence(version: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = version.split('.').collect();

    if parts.is_empty() {
        return None;
    }

    let mut numbers = Vec::with_capacity(parts.len());
    for part in parts {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }

        numbers.push(part.parse().ok()?);
    }

    Some(numbers)
}

fn parse_date_like(version: &str) -> Option<(u32, u32, u32)> {
    if version.len() == 8 && version.bytes().all(|byte| byte.is_ascii_digit()) {
        let year = version[0..4].parse().ok()?;
        let month = version[4..6].parse().ok()?;
        let day = version[6..8].parse().ok()?;

        return valid_date(year, month, day);
    }

    if version.len() == 10 {
        let separator = version.as_bytes()[4];
        if (separator == b'-' || separator == b'.')
            && version.as_bytes()[7] == separator
            && version
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        {
            let year = version[0..4].parse().ok()?;
            let month = version[5..7].parse().ok()?;
            let day = version[8..10].parse().ok()?;

            return valid_date(year, month, day);
        }
    }

    None
}

fn looks_date_like(version: &str) -> bool {
    if version.len() == 8 {
        return version.bytes().all(|byte| byte.is_ascii_digit());
    }

    if version.len() == 10 {
        let bytes = version.as_bytes();
        return (bytes[4] == b'-' || bytes[4] == b'.')
            && bytes[7] == bytes[4]
            && version
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    }

    false
}

fn valid_date(year: u32, month: u32, day: u32) -> Option<(u32, u32, u32)> {
    if year == 0 || !(1..=12).contains(&month) {
        return None;
    }

    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };

    if (1..=max_day).contains(&day) {
        Some((year, month, day))
    } else {
        None
    }
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    use super::{compare_versions, VersionComparison};
    use crate::PackageVersion;

    fn compare(current: &str, candidate: &str) -> VersionComparison {
        compare_versions(
            &PackageVersion::new(current),
            &PackageVersion::new(candidate),
        )
    }

    #[test]
    fn package_versions_preserve_original_source_strings() {
        let version = PackageVersion::new("v4.1-beta");

        assert_eq!(version.as_str(), "v4.1-beta");
    }

    #[test]
    fn versions_compare_equal_after_v_prefix_normalization() {
        assert_eq!(compare("v1.2.3", "1.2.3"), VersionComparison::Equal);
    }

    #[test]
    fn versions_compare_semver_like_versions() {
        assert_eq!(
            compare("1.2.3", "1.3.0"),
            VersionComparison::CandidateIsNewer
        );
        assert_eq!(compare("2.0.0", "1.9.9"), VersionComparison::CurrentIsNewer);
        assert_eq!(
            compare("1.0.0-alpha", "1.0.0"),
            VersionComparison::CandidateIsNewer
        );
    }

    #[test]
    fn versions_ignore_semver_build_metadata_for_precedence() {
        assert_eq!(compare("1.0.0+abc", "1.0.0+xyz"), VersionComparison::Equal);
    }

    #[test]
    fn versions_compare_numeric_sequences() {
        assert_eq!(
            compare("2.304", "2.305"),
            VersionComparison::CandidateIsNewer
        );
        assert_eq!(compare("10", "9"), VersionComparison::CurrentIsNewer);
    }

    #[test]
    fn versions_compare_date_like_versions() {
        assert_eq!(
            compare("2024-06-01", "2024-07-01"),
            VersionComparison::CandidateIsNewer
        );
        assert_eq!(
            compare("20240601", "20240531"),
            VersionComparison::CurrentIsNewer
        );
    }

    #[test]
    fn versions_return_unknown_for_ambiguous_versions() {
        for (current, candidate) in [
            ("latest", "stable"),
            ("release-a", "release-b"),
            ("1.0-custom", "1.0-final"),
            ("2024-13-01", "2024-12-01"),
        ] {
            assert_eq!(
                compare(current, candidate),
                VersionComparison::Unknown,
                "{current:?} vs {candidate:?} should be unknown"
            );
        }
    }
}
