#[cfg(any(target_os = "linux", test))]
use std::io::Read as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sample {
    pub rss_bytes: u64,
    pub peak_rss_bytes: u64,
}

pub(super) fn sample() -> Result<Sample, String> {
    Ok(Sample {
        rss_bytes: current_rss()?,
        peak_rss_bytes: peak_rss()?,
    })
}

#[cfg(target_os = "linux")]
fn current_rss() -> Result<u64, String> {
    let mut text = String::new();
    read_statm_file(std::path::Path::new("/proc/self/statm"), &mut text)
        .map_err(|error| format!("read /proc/self/statm: {error}"))?;
    let pages = parse_statm(&text)?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    rss_from_pages(pages, page_size)
}

#[cfg(any(target_os = "linux", test))]
fn read_statm_file(path: &std::path::Path, text: &mut String) -> std::io::Result<usize> {
    std::fs::File::open(path)?.take(4096).read_to_string(text)
}

#[cfg(target_os = "macos")]
fn current_rss() -> Result<u64, String> {
    use std::mem::MaybeUninit;
    let mut info = MaybeUninit::<libc::mach_task_basic_info>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
    }
    let status = unsafe {
        libc::task_info(
            mach_task_self_,
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    let info = unsafe { info.assume_init() };
    map_task_info(status, count, info.resident_size)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_rss() -> Result<u64, String> {
    Err("memory profiling is unsupported on this operating system".into())
}

#[cfg(target_os = "macos")]
fn map_task_info(status: libc::kern_return_t, count: u32, resident: u64) -> Result<u64, String> {
    if status != 0 {
        return Err(format!("task_info failed with status {status}"));
    }
    if count != libc::MACH_TASK_BASIC_INFO_COUNT {
        return Err("task_info returned an unexpected value count".into());
    }
    Ok(resident)
}

fn peak_rss() -> Result<u64, String> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return Err(format!(
            "getrusage failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let value = unsafe { usage.assume_init() }.ru_maxrss;
    normalize_peak(value)
}

#[cfg(target_os = "linux")]
fn normalize_peak(value: libc::c_long) -> Result<u64, String> {
    normalize_peak_value(value, true)
}

#[cfg(target_os = "macos")]
fn normalize_peak(value: libc::c_long) -> Result<u64, String> {
    normalize_peak_value(value, false)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn normalize_peak(_value: libc::c_long) -> Result<u64, String> {
    Err("memory profiling is unsupported on this operating system".into())
}

fn normalize_peak_value(value: libc::c_long, kibibytes: bool) -> Result<u64, String> {
    let value = u64::try_from(value).map_err(|_| "ru_maxrss was negative".to_string())?;
    if kibibytes {
        value
            .checked_mul(1024)
            .ok_or_else(|| "ru_maxrss byte conversion overflow".into())
    } else {
        Ok(value)
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_statm(text: &str) -> Result<u64, String> {
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 7 {
        return Err("/proc/self/statm has an unexpected field count".into());
    }
    fields[1]
        .parse::<u64>()
        .map_err(|_| "/proc/self/statm resident pages are invalid".into())
}

#[cfg(any(target_os = "linux", test))]
fn rss_from_pages(pages: u64, page_size: libc::c_long) -> Result<u64, String> {
    let page_size = u64::try_from(page_size).map_err(|_| "page-size query failed".to_string())?;
    if page_size == 0 {
        return Err("page-size query returned zero".into());
    }
    pages
        .checked_mul(page_size)
        .ok_or_else(|| "resident byte conversion overflow".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_linux_statm_shape() {
        assert_eq!(parse_statm("100 42 3 4 5 6 7\n"), Ok(42));
        assert!(parse_statm("100 bad 3 4 5 6 7").is_err());
        assert!(parse_statm("100 42").is_err());
        assert!(parse_statm("").is_err());
    }

    #[test]
    fn absent_statm_file_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        assert!(read_statm_file(&temp.path().join("absent"), &mut String::new()).is_err());
    }

    #[test]
    fn page_conversion_is_checked() {
        assert_eq!(rss_from_pages(3, 4096), Ok(12_288));
        assert!(rss_from_pages(1, -1).is_err());
        assert!(rss_from_pages(1, 0).is_err());
        assert!(rss_from_pages(u64::MAX, 2).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn task_info_mapping_checks_status_and_count() {
        assert_eq!(
            map_task_info(0, libc::MACH_TASK_BASIC_INFO_COUNT, 99),
            Ok(99)
        );
        assert!(map_task_info(1, libc::MACH_TASK_BASIC_INFO_COUNT, 99).is_err());
        assert!(map_task_info(0, 0, 99).is_err());
    }

    #[test]
    fn peak_units_and_sign_are_checked() {
        assert_eq!(normalize_peak_value(12, true), Ok(12 * 1024));
        assert_eq!(normalize_peak_value(12, false), Ok(12));
        assert!(normalize_peak_value(-1, false).is_err());
    }
}
