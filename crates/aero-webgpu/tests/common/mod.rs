pub fn require_webgpu() -> bool {
    matches!(
        std::env::var("AERO_REQUIRE_WEBGPU")
            .as_deref()
            .map(str::trim),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES") | Ok("on") | Ok("ON")
    )
}

pub fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU=1 but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}
