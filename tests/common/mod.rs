use std::ffi::OsStr;

pub fn net_tests_enabled() -> bool {
    net_tests_enabled_for(std::env::var_os("NAB_NET_TESTS").as_deref())
}

pub fn net_tests_enabled_for(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.to_string_lossy().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}
